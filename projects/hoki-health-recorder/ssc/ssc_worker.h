#ifndef SSC_WORKER_H
#define SSC_WORKER_H
#include "ssc_journal.h"
#include <pthread.h>
#include <stdlib.h>
#include "ssc_status.h"

#define SW_CAPACITY 4096
#define SW_BYTE_CAPACITY (4u*1024u*1024u)
struct sw_record {
    unsigned kind,id;
    size_t len,offset;
    uint64_t boot,real;
};
/* Hooks return 0 or a positive errno and run under the queue mutex. Their
 * implementation must use a distinct kernel wake-lock name for this collector.
 * Suspend still requires the kernel wakeup_count handshake. */
struct ssc_worker {
    struct ssc_journal journal;
    struct ssc_status status;
    pthread_mutex_t mutex;
    pthread_cond_t ready;
    pthread_t thread;
    struct sw_record queue[SW_CAPACITY];
    unsigned char bytes[SW_BYTE_CAPACITY];
    size_t byte_tail,byte_count,byte_high_water;
    unsigned head,count,high_water;
    uint64_t accepted,durable,rejected;
    uint64_t sync_target;
    uint64_t last_progress_ns;
    int error,stopping;
    int (*hold)(void *);
    int (*release)(void *);
    void *context;
};
static void *sw_run(void *arg) {
    struct ssc_worker *w=arg;
    /* One in-flight slot in addition to the bounded queue slots. */
    struct sw_record record;
    uint64_t written=0;
    unsigned char payload[65536];
    pthread_mutex_lock(&w->mutex);
    for(;;) {
        while(!w->count && !w->stopping && !w->error)
            pthread_cond_wait(&w->ready,&w->mutex);
        if(w->error || (!w->count && w->stopping)) break;
        record=w->queue[w->head];
        size_t first=SW_BYTE_CAPACITY-record.offset;
        if(first>record.len)first=record.len;
        if(first)memcpy(payload,w->bytes+record.offset,first);
        if(record.len>first)memcpy(payload+first,w->bytes,record.len-first);
        w->byte_count-=record.len;
        w->head=(w->head+1)%SW_CAPACITY; --w->count;
        pthread_mutex_unlock(&w->mutex);
        int rc=sj_append_at(&w->journal,record.kind,record.id,payload,
                            record.len,record.boot,record.real);
        int error=rc?errno:0;
        pthread_mutex_lock(&w->mutex);
        if(error) { if(!w->error) w->error=error; break; }
        ++written;
        /* A command barrier only needs its accepted prefix. Later producer
         * traffic must not force it to wait for the entire queue to empty. */
        if(w->count && (w->sync_target<=w->durable || written<w->sync_target)) continue;
        uint64_t boundary=written;
        pthread_mutex_unlock(&w->mutex);
        rc=fsync(w->journal.fd);
        error=rc?errno:0;
        pthread_mutex_lock(&w->mutex);
        if(error) { if(!w->error) w->error=error; break; }
        w->durable=boundary;
        /* At most once per five minutes, piggyback on an existing data fsync.
         * No timer and no extra wake. Keep producers unblocked during status I/O.
         * Retain the existing wake hold until both data and snapshot are durable. */
        struct timespec stamp;
        if(clock_gettime(CLOCK_BOOTTIME,&stamp)) { w->error=errno;break; }
        uint64_t now=(uint64_t)stamp.tv_sec*1000000000+stamp.tv_nsec;
        if(!w->error && !w->stopping &&
           (!w->status.progress_initialized || now-w->last_progress_ns>=UINT64_C(300000000000))) {
            uint64_t accepted=w->accepted,durable=w->durable,rejected=w->rejected;
            unsigned high_water=w->high_water;
            pthread_mutex_unlock(&w->mutex);
            error=ss_progress(&w->status,accepted,durable,rejected,high_water,SW_CAPACITY);
            pthread_mutex_lock(&w->mutex);
            if(error) { if(!w->error)w->error=error;break; }
            w->last_progress_ns=now;
        }
        pthread_cond_broadcast(&w->ready);
        /* New enqueue during fsync remains covered by the existing hold. */
        if(!w->error && !w->stopping && w->accepted==w->durable) {
            error=w->release(w->context);
            if(error) w->error=error;
        }
    }
    pthread_cond_broadcast(&w->ready);
    pthread_mutex_unlock(&w->mutex);
    return NULL;
}
static int sw_start(struct ssc_worker *w,const char *directory,
                    uint64_t limit,uint64_t reserve,
                    int (*hold)(void *),int (*release)(void *),void *context) {
    memset(w,0,sizeof *w);
    w->status.directory=-1;
    if(!hold || !release) return EINVAL;
    w->hold=hold;w->release=release;w->context=context;
    int rc=pthread_mutex_init(&w->mutex,NULL);
    if(rc) return rc;
    pthread_condattr_t attr;
    rc=pthread_condattr_init(&attr);
    if(rc) { pthread_mutex_destroy(&w->mutex); return rc; }
    rc=pthread_condattr_setclock(&attr,CLOCK_MONOTONIC);
    if(!rc) rc=pthread_cond_init(&w->ready,&attr);
    pthread_condattr_destroy(&attr);
    if(rc) { pthread_mutex_destroy(&w->mutex); return rc; }
    rc=hold(context);
    if(!rc && sj_open(&w->journal,directory,limit,reserve)) {
        rc=errno; sj_abort(&w->journal);
    }
    if(!rc) rc=ss_open(&w->status,w->journal.dir,SW_CAPACITY);
    if(!rc) rc=release(context);
    if(!rc) rc=pthread_create(&w->thread,NULL,sw_run,w);
    if(rc) {
        /* Contract has not started; caller supervises ambiguous hook failure. */
        if(w->journal.limit) sj_abort(&w->journal);
        ss_close(&w->status);
        release(context);
        pthread_cond_destroy(&w->ready);pthread_mutex_destroy(&w->mutex);
    }
    return rc;
}
static int sw_push(struct ssc_worker *w,unsigned kind,unsigned id,
                   const void *data,size_t len) {
    struct timespec boot,real;
    int clock_error=0;
    if(clock_gettime(CLOCK_BOOTTIME,&boot) || clock_gettime(CLOCK_REALTIME,&real))
        clock_error=errno;
    pthread_mutex_lock(&w->mutex);
    int error=w->error;
    if(!error && w->stopping) error=ESHUTDOWN;
    if(!error) error=w->hold(w->context);
    if(!error) error=clock_error;
    if(!error && (kind<1 || kind>4 || len>65536 || (!data&&len))) error=EINVAL;
    if(!error && (w->count==SW_CAPACITY || len>SW_BYTE_CAPACITY-w->byte_count)) error=ENOBUFS;
    if(error) {
        ++w->rejected;
        if(!w->error) w->error=error;
    } else {
        struct sw_record *r=&w->queue[(w->head+w->count)%SW_CAPACITY];
        r->kind=kind;r->id=id;r->len=len;
        r->boot=(uint64_t)boot.tv_sec*1000000000+boot.tv_nsec;
        r->real=(uint64_t)real.tv_sec*1000000000+real.tv_nsec;
        r->offset=w->byte_tail;
        size_t first=SW_BYTE_CAPACITY-w->byte_tail;
        if(first>len)first=len;
        if(first)memcpy(w->bytes+w->byte_tail,data,first);
        if(len>first)memcpy(w->bytes,(const unsigned char *)data+first,len-first);
        w->byte_tail=(w->byte_tail+len)%SW_BYTE_CAPACITY;
        w->byte_count+=len;
        if(w->byte_count>w->byte_high_water)w->byte_high_water=w->byte_count;
        ++w->count;++w->accepted;
        if(w->count>w->high_water)w->high_water=w->count;
    }
    pthread_cond_broadcast(&w->ready);
    pthread_mutex_unlock(&w->mutex);
    return error;
}
/* Main-thread write-ahead barrier. Call after enqueueing an intent and before
 * sending it. Wait only for the accepted prefix at entry, never hold up producers.
 * A timeout/error poisons the session: no subsequent command may be sent using
 * this journal. Supervisor still bounds teardown if storage itself is stuck.
 * Must not run concurrently with sw_finish (which destroys the mutex/cond). */
static int sw_barrier(struct ssc_worker *w,unsigned timeout_ms) {
    struct timespec deadline;
    if(clock_gettime(CLOCK_MONOTONIC,&deadline)) return errno;
    deadline.tv_sec+=timeout_ms/1000;
    deadline.tv_nsec+=(long)(timeout_ms%1000)*1000000;
    if(deadline.tv_nsec>=1000000000) {
        ++deadline.tv_sec;deadline.tv_nsec-=1000000000;
    }
    pthread_mutex_lock(&w->mutex);
    uint64_t boundary=w->accepted;
    if(boundary>w->sync_target)w->sync_target=boundary;
    int error=w->error;
    while(!error && !w->stopping && w->durable<boundary) {
        int rc=pthread_cond_timedwait(&w->ready,&w->mutex,&deadline);
        error=w->error;
        if(!error && rc && w->durable<boundary) error=rc;
    }
    if(!error && w->stopping) error=ESHUTDOWN;
    if(error && !w->error) w->error=error;
    pthread_cond_broadcast(&w->ready);
    pthread_mutex_unlock(&w->mutex);
    return error;
}
/* Producers must be joined/stopped before this call. Failure retains a prefix
 * and omits the normal footer. Orderly teardown ends the wake-hold contract;
 * abnormal death instead requires supervisor cleanup of the kernel hold. */
static int sw_finish(struct ssc_worker *w) {
    pthread_mutex_lock(&w->mutex);
    int hold_error=w->hold(w->context);
    if(hold_error && !w->error) w->error=hold_error;
    w->stopping=1;
    pthread_cond_broadcast(&w->ready);
    pthread_mutex_unlock(&w->mutex);
    pthread_join(w->thread,NULL);
    int result=w->error;
    if(!result && sj_finish(&w->journal)) result=errno;
    else if(result) sj_abort(&w->journal);
    int status_error=ss_publish(&w->status,1,result,w->accepted,w->durable,
                                w->rejected,w->high_water,SW_CAPACITY);
    if(!result) result=status_error;
    ss_close(&w->status);
    int release_error=w->release(w->context);
    if(!result) result=release_error;
    pthread_cond_destroy(&w->ready);pthread_mutex_destroy(&w->mutex);
    return result;
}
#endif
