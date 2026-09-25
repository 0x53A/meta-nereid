#define _GNU_SOURCE
#include "ssc_worker.h"
#include <assert.h>
#include <stdatomic.h>
#include <stdio.h>
#include <sys/stat.h>

static pthread_mutex_t gate=PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t gate_changed=PTHREAD_COND_INITIALIZER;
static int block_sync,entered,proceed,fail_sync,progress_only;
static atomic_int held,unlocks;
static _Atomic(struct ssc_worker *) replenished_worker;
static atomic_int replenishments,barrier_waiting,first_replenished_sync;
ssize_t __real_write(int,const void *,size_t);
ssize_t __wrap_write(int fd,const void *data,size_t size) {
    ssize_t result=__real_write(fd,data,size);
    struct ssc_worker *w=atomic_load(&replenished_worker);
    if(w && fd==w->journal.fd && size==40 && result==40
       && atomic_load(&replenishments)<64) {
        assert(!sw_push(w,3,33,"ongoing",7));
        atomic_fetch_add(&replenishments,1);
    }
    return result;
}
int __real_pthread_cond_timedwait(pthread_cond_t *,pthread_mutex_t *,const struct timespec *);
int __wrap_pthread_cond_timedwait(pthread_cond_t *cond,pthread_mutex_t *mutex,const struct timespec *time) {
    atomic_store(&barrier_waiting,1);
    return __real_pthread_cond_timedwait(cond,mutex,time);
}
int __real_fsync(int);
int __wrap_fsync(int fd) {
    struct ssc_worker *w=atomic_load(&replenished_worker);
    if(w && fd==w->journal.fd) {
        int unset=-1;
        atomic_compare_exchange_strong(&first_replenished_sync,&unset,
                                       atomic_load(&replenishments));
    }
    char link[64],target[512];
    snprintf(link,sizeof link,"/proc/self/fd/%d",fd);
    ssize_t size=readlink(link,target,sizeof target-1);assert(size>=0);
    target[size]=0;
    pthread_mutex_lock(&gate);
    int selected=!progress_only || strstr(target,"/progress.pending")!=NULL;
    if(block_sync && selected) {
        entered=1;pthread_cond_broadcast(&gate_changed);
        while(!proceed) pthread_cond_wait(&gate_changed,&gate);
    }
    int failure=fail_sync && selected;
    pthread_mutex_unlock(&gate);
    if(failure) { errno=EIO;return -1; }
    return __real_fsync(fd);
}
static int hold(void *unused) { (void)unused;atomic_store(&held,1);return 0; }
static int release(void *unused) {
    struct ssc_worker *w=atomic_load(&replenished_worker);
    if(w)assert(w->accepted==w->durable);
    (void)unused;atomic_store(&held,0);atomic_fetch_add(&unlocks,1);return 0;
}
static void arm(void) {
    pthread_mutex_lock(&gate);block_sync=1;entered=proceed=0;
    pthread_mutex_unlock(&gate);
}
static void wait_entered(void) {
    pthread_mutex_lock(&gate);
    while(!entered) pthread_cond_wait(&gate_changed,&gate);
    pthread_mutex_unlock(&gate);
}
static void unblock(int fail) {
    pthread_mutex_lock(&gate);block_sync=0;proceed=1;fail_sync=fail;
    pthread_cond_broadcast(&gate_changed);pthread_mutex_unlock(&gate);
}
static struct ssc_worker *start(char *directory) {
    assert(mkdtemp(directory));
    struct ssc_worker *w=calloc(1,sizeof *w);assert(w);
    assert(sw_start(w,directory,16*1048576,0,hold,release,NULL)==0);
    return w;
}
static atomic_int barrier_done;
static int barrier_result;
static void *barrier_thread(void *arg) {
    barrier_result=sw_barrier(arg,2000);
    atomic_store(&barrier_done,1);
    return NULL;
}
int main(void) {
    char directory[]="/tmp/hoki-ssc-worker-XXXXXX";
    struct ssc_worker *w=start(directory);
    int before=atomic_load(&unlocks);
    arm();assert(sw_push(w,3,33,"first",5)==0);wait_entered();
    assert(atomic_load(&held));
    /* Enqueue must finish while fsync is blocked on a different thread. */
    assert(sw_push(w,3,33,"second",6)==0);
    pthread_mutex_lock(&w->mutex);
    assert(w->accepted==2 && w->durable==0);
    pthread_mutex_unlock(&w->mutex);
    assert(atomic_load(&unlocks)==before);
    pthread_t waiter;assert(!pthread_create(&waiter,NULL,barrier_thread,w));
    struct timespec delay={0,20000000};nanosleep(&delay,NULL);
    assert(!atomic_load(&barrier_done));
    unblock(0);assert(!pthread_join(waiter,NULL));
    assert(barrier_result==0 && atomic_load(&barrier_done));
    assert(sw_barrier(w,100)==0);assert(sw_finish(w)==0);
    assert(w->accepted==2 && w->durable==2 && !w->rejected);
    assert(!atomic_load(&held));free(w);
    printf("%s/events.ssc\n",directory);

    /* Keep the queue nonempty after every journal append. A barrier must sync
     * its already-written prefix without first draining these later arrivals. */
    char sustained[]="/tmp/hoki-ssc-sustained-XXXXXX";
    w=start(sustained);arm();assert(!sw_push(w,3,33,"first",5));wait_entered();
    assert(!sw_push(w,1,32,"intent",6));
    atomic_store(&barrier_done,0);atomic_store(&barrier_waiting,0);
    atomic_store(&first_replenished_sync,-1);
    atomic_store(&replenished_worker,w);
    assert(!pthread_create(&waiter,NULL,barrier_thread,w));
    while(!atomic_load(&barrier_waiting)) sched_yield();
    unblock(0);assert(!pthread_join(waiter,NULL));
    assert(barrier_result==0);
    /* Examine the first fsync boundary, rather than scheduler-dependent total
     * producer work after the barrier thread wakes. */
    atomic_store(&replenished_worker,NULL);
    assert(atomic_load(&first_replenished_sync)==1);
    assert(!sw_finish(w));free(w);

    char overflow[]="/tmp/hoki-ssc-overflow-XXXXXX";
    w=start(overflow);arm();assert(sw_push(w,3,33,"x",1)==0);wait_entered();
    for(unsigned i=0;i<SW_CAPACITY;i++) assert(sw_push(w,3,33,"x",1)==0);
    assert(sw_push(w,3,33,"x",1)==ENOBUFS);
    assert(w->rejected==1 && w->high_water==SW_CAPACITY && atomic_load(&held));
    unblock(0);assert(sw_finish(w)==ENOBUFS);free(w);

    char failure[]="/tmp/hoki-ssc-failure-XXXXXX";
    w=start(failure);arm();assert(sw_push(w,3,33,"x",1)==0);wait_entered();
    unblock(1);assert(sw_barrier(w,2000)==EIO);assert(sw_finish(w)==EIO);
    assert(w->durable==0);free(w);
    fail_sync=0;
    char timeout[]="/tmp/hoki-ssc-timeout-XXXXXX";
    w=start(timeout);arm();assert(sw_push(w,1,32,"intent",6)==0);wait_entered();
    assert(sw_barrier(w,20)==ETIMEDOUT);
    assert(w->durable==0);
    assert(sw_push(w,1,32,"later",5)==ETIMEDOUT);
    unblock(0);assert(sw_finish(w)==ETIMEDOUT);free(w);
    /* Data arriving just after a snapshot does not cause another status fsync.
     * Final status remains independently authoritative after stopping. */
    char progress_dir[]="/tmp/hoki-ssc-progress-XXXXXX";
    w=start(progress_dir);
    assert(!sw_push(w,3,33,"first",5));assert(!sw_barrier(w,2000));
    assert(!sw_push(w,3,33,"second",6));assert(!sw_barrier(w,2000));
    assert(!sw_finish(w));
    char path[256],snapshot[768];
    snprintf(path,sizeof path,"%s/progress.json",progress_dir);
    FILE *f=fopen(path,"r");assert(f);size_t n=fread(snapshot,1,sizeof snapshot-1,f);
    snapshot[n]=0;assert(!fclose(f));
    assert(strstr(snapshot,"\"phase\":\"recording\""));
    assert(strstr(snapshot,"\"durable\":1"));
    snprintf(path,sizeof path,"%s/status.json",progress_dir);
    f=fopen(path,"r");assert(f);n=fread(snapshot,1,sizeof snapshot-1,f);
    snapshot[n]=0;assert(!fclose(f));
    assert(strstr(snapshot,"\"phase\":\"closed\""));
    assert(strstr(snapshot,"\"durable\":2"));free(w);
    /* A blocked progress-file fsync must not block producers or drop the wake
     * hold. Its failure must propagate even though the first data record synced. */
    char progress_failure[]="/tmp/hoki-ssc-progress-failure-XXXXXX";
    w=start(progress_failure);before=atomic_load(&unlocks);
    progress_only=1;arm();
    assert(!sw_push(w,3,33,"durable",7));wait_entered();
    assert(!sw_push(w,3,33,"queued",6));
    pthread_mutex_lock(&w->mutex);
    assert(w->accepted==2 && w->durable==1);
    pthread_mutex_unlock(&w->mutex);
    assert(atomic_load(&held) && atomic_load(&unlocks)==before);
    unblock(1);assert(sw_barrier(w,2000)==EIO);
    assert(atomic_load(&held));assert(sw_finish(w)==EIO);
    assert(!atomic_load(&held));
    snprintf(path,sizeof path,"%s/progress.json",progress_failure);
    assert(access(path,F_OK) && errno==ENOENT);
    snprintf(path,sizeof path,"%s/status.json",progress_failure);
    f=fopen(path,"r");assert(f);n=fread(snapshot,1,sizeof snapshot-1,f);
    snapshot[n]=0;assert(!fclose(f));
    assert(strstr(snapshot,"\"archive_complete\":false"));
    assert(strstr(snapshot,"\"archive_error\":5"));
    assert(strstr(snapshot,"\"accepted_not_confirmed_durable\":1"));
    snprintf(path,sizeof path,"%s/events.ssc",progress_failure);
    struct stat st;assert(!stat(path,&st));
    /* Exactly the durable first record: no queued second record or clean footer. */
    assert(st.st_size==16+40+7);
    assert(w->accepted==2 && w->durable==1);free(w);
    progress_only=fail_sync=0;
    /* Reproduce small-message burst pressure while disk sync cannot complete.
     * The previous 64-slot queue would fail this at its 65th queued message. */
    char burst_dir[]="/tmp/hoki-ssc-burst-XXXXXX";
    unsigned char small[139];for(unsigned i=0;i<sizeof small;i++)small[i]=(unsigned char)i;
    w=start(burst_dir);arm();assert(!sw_push(w,3,33,small,sizeof small));wait_entered();
    for(unsigned i=0;i<512;i++)assert(!sw_push(w,3,33,small,sizeof small));
    pthread_mutex_lock(&w->mutex);
    assert(w->count==512 && w->byte_count==512*sizeof small && w->high_water==512);
    pthread_mutex_unlock(&w->mutex);
    assert(atomic_load(&held));unblock(0);
    assert(!sw_barrier(w,2000));
    /* Drain/refill enough batches to wrap the descriptor ring as well. */
    for(unsigned batch=0;batch<8;batch++) {
        for(unsigned i=0;i<512;i++)assert(!sw_push(w,3,33,small,sizeof small));
        assert(!sw_barrier(w,2000));
    }
    assert(!sw_finish(w));
    assert(w->accepted==4609 && w->durable==4609 && w->rejected==0);free(w);

    /* An empty arena can start at any ring offset. Force a split copy then
     * verify exact bytes after draining, including a legal empty payload. */
    char wrap_dir[]="/tmp/hoki-ssc-wrap-XXXXXX";
    w=start(wrap_dir);pthread_mutex_lock(&w->mutex);
    w->byte_tail=SW_BYTE_CAPACITY-31;pthread_mutex_unlock(&w->mutex);
    assert(!sw_push(w,3,33,small,sizeof small));
    assert(!sw_push(w,4,0,NULL,0));assert(!sw_barrier(w,2000));assert(!sw_finish(w));
    snprintf(path,sizeof path,"%s/events.ssc",wrap_dir);
    f=fopen(path,"rb");assert(f);unsigned char raw[16+40+139+40+40];
    assert(fread(raw,1,sizeof raw,f)==sizeof raw && fgetc(f)==EOF);assert(!fclose(f));
    assert(!memcmp(raw+16+40,small,sizeof small));
    assert(raw[16+40+139+4]==4 && raw[16+40+139+40+4]==255);
    assert(!w->byte_count);free(w);

    /* Descriptor headroom never removes the independent 4MiB payload limit. */
    char bytes_dir[]="/tmp/hoki-ssc-byte-limit-XXXXXX";
    unsigned char large[65536];memset(large,0xa5,sizeof large);
    w=start(bytes_dir);arm();assert(!sw_push(w,3,33,small,sizeof small));wait_entered();
    for(unsigned i=0;i<SW_BYTE_CAPACITY/sizeof large;i++)assert(!sw_push(w,3,33,large,sizeof large));
    assert(sw_push(w,3,33,small,1)==ENOBUFS);
    assert(w->byte_count==SW_BYTE_CAPACITY && w->count<SW_CAPACITY && w->rejected==1);
    unblock(0);assert(sw_finish(w)==ENOBUFS);free(w);
    puts("queue/fsync race, overflow, prefix barrier under sustained input, progress failure/wake race and poisoned timeout passed");
}
