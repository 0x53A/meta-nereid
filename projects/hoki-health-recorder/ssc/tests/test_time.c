#include <assert.h>
#include <stdio.h>
#include <string.h>
#include "time_payload.h"
int main(void) {
 unsigned char out[32];size_t size=0;
 const int offsets[]={0,7200,-28800,50400,-43200};
 /* Independent wire fixtures: protobuf int32 negative hours use ten bytes,
  * not zigzag encoding, unsigned 32-bit encoding or offset seconds. */
 const char *expected[]={
  "420a0880c8c3d50610001800",
  "420a0880c8c3d50610001802",
  "42130880c8c3d506100018f8ffffffffffffffff01",
  "420a0880c8c3d5061000180e",
  "42130880c8c3d506100018f4ffffffffffffffff01"
 };
 for(size_t i=0;i<sizeof offsets/sizeof offsets[0];i++) {
  assert(!ssc_time_payload(out,sizeof out,1789977600,offsets[i],&size));
  char encoded[65];
  for(size_t j=0;j<size;j++)snprintf(encoded+j*2,3,"%02x",out[j]);
  assert(!strcmp(encoded,expected[i]));
 }
 assert(ssc_time_payload(out,sizeof out,1789977600,19800,&size));
 assert(ssc_time_payload(out,sizeof out,1789977600,54000,&size));
 assert(ssc_time_payload(out,1,1789977600,0,&size));
 assert(ssc_time_payload(out,sizeof out,0,0,&size));
 unsigned char get[40];size_t got=0;
 assert(!ssc_time_payload(out,sizeof out,1789977600,-28800,&size));
 assert(!ssc_minute_time_payload(get,sizeof get,1789977600,-28800,&got));
 assert(got==size+2 && get[0]==8 && get[1]==1 && get[2]==18);
 assert(!memcmp(get+3,out+1,size-1));
 assert(ssc_minute_time_payload(get,got-1,1789977600,-28800,&got));
 assert(ssc_minute_time_payload(get,sizeof get,0,0,&got));
 assert(ssc_minute_time_payload(NULL,40,1789977600,0,&got));
 puts("five golden timezone encodings, clock request nesting and invalid inputs passed");
 return 0;
}
