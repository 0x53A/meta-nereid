#include "ssc_extra_reads.h"
#include "ssc_wire.h"
#include <assert.h>
int main(void) {
 unsigned request=0,reply=0;unsigned char payload[8];size_t size=99;
 assert(!ser_plan("--chrm-config",&request,&reply,payload,&size));
 assert(request==775 && reply==775 && size==0);
 assert(!ser_plan("--tracker-config",&request,&reply,payload,&size));
 assert(request==768 && reply==1029 && size==6);
 struct mt_field id,state,body;
 assert(mt_get(payload,size,1,0,&id)==1 && id.value==1);
 assert(mt_get(payload,size,2,0,&state)==1 && state.value==0);
 assert(mt_get(payload,size,3,2,&body)==1 && body.len==0);
 const unsigned char *next=payload,*end=payload+size;struct mt_field field;
 unsigned count=0;int rc;
 while((rc=mt_next(&next,end,&field))>0)count++;
 assert(rc==0 && count==3);
 assert(ser_plan("--set-chrm",&request,&reply,payload,&size));
 assert(ser_plan("--tracker-start",&request,&reply,payload,&size));
 assert(ser_plan("--attributes",&request,&reply,payload,&size));
 return 0;
}
