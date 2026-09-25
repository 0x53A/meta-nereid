#ifndef MINUTE_TRACKER_H
#define MINUTE_TRACKER_H
#include <stddef.h>
#include <stdint.h>
#include <string.h>

#include "ssc_wire.h"
struct minute_tracker {
    uint64_t low,high,bytes;
    uint32_t last;
    unsigned chunks,completed;
    int pending,error;
};
static int mt_init(struct minute_tracker *t,const unsigned char *suid,size_t n) {
    struct mt_field a,b;
    memset(t,0,sizeof *t);
    if(mt_get(suid,n,1,1,&a)!=1 || mt_get(suid,n,2,1,&b)!=1)return -1;
    t->low=a.value;t->high=b.value;return 0;
}
static int mt_begin(struct minute_tracker *t) {
    if(t->pending || t->error) { t->error=1;return -1; }
    t->pending=1;t->chunks=0;t->bytes=0;return 0;
}
/* Caller serializes begin/feed/status. No deletion or ACK is generated.
 * EOP is a protocol marker, not durable completion or file-integrity proof. */
static int mt_envelope(struct minute_tracker *t,const unsigned char *p,size_t len) {
    struct mt_field suid,low,high,event,id,ticks,payload,buffer,tx,eof,eop;
    if(mt_get(p,len,1,2,&suid)!=1 ||
       mt_get(suid.data,suid.len,1,1,&low)!=1 ||
       mt_get(suid.data,suid.len,2,1,&high)!=1)return -1;
    if(low.value!=t->low || high.value!=t->high)return 0;
    const unsigned char *end=p+len;int rc;
    while((rc=mt_next(&p,end,&event))>0) {
        if(event.number!=2)continue;
        if(event.wire!=2 || mt_get(event.data,event.len,1,5,&id)!=1 ||
           mt_get(event.data,event.len,2,1,&ticks)!=1 ||
           mt_get(event.data,event.len,3,2,&payload)!=1)return -1;
        if(id.value!=1028)continue;
        if(!t->pending || mt_get(payload.data,payload.len,1,2,&buffer)!=1 ||
           mt_get(payload.data,payload.len,2,0,&tx)!=1 ||
           mt_get(payload.data,payload.len,3,0,&eof)!=1 ||
           mt_get(payload.data,payload.len,4,0,&eop)!=1)return -1;
        if(tx.value>UINT32_MAX || eof.value>1 || eop.value>1 ||
           (t->chunks && (uint32_t)tx.value!=(uint32_t)(t->last+1u)) ||
           t->chunks>=65536 || buffer.len>16*1024*1024-t->bytes ||
           (eop.value && !eof.value))return -1;
        t->last=(uint32_t)tx.value;++t->chunks;t->bytes+=buffer.len;
        if(eop.value) { t->pending=0;++t->completed; }
    }
    return rc<0?-1:0;
}
static int mt_feed(struct minute_tracker *t,unsigned message,const void *raw,size_t len) {
    if(t->error)return -1;
    if(message!=33 && message!=34)return 0;
    const unsigned char *p=raw;size_t pos=0;int found=0;
    if(len>65536)goto bad;
    while(pos<len) {
        if(len-pos<3)goto bad;
        unsigned tag=p[pos],size=p[pos+1]|((unsigned)p[pos+2]<<8);pos+=3;
        if(size>len-pos)goto bad;
        if(tag==2) {
            if(found++ || size<2 || (p[pos]|((unsigned)p[pos+1]<<8))!=size-2 ||
               mt_envelope(t,p+pos+2,size-2))goto bad;
        }
        pos+=size;
    }
    if(found==1)return 0;
bad:
    t->error=1;return -1;
}
#endif
