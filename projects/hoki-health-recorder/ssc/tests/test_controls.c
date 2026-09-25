#include "ssc_controls.h"
#include <assert.h>
int main(void) {
 unsigned message;unsigned char out[8];size_t n;
 assert(!sctl_payload("--set-tracking","1",NULL,NULL,&message,out,&n));assert(message==776 && n==2 && !memcmp(out,"\x08\x01",2));
 assert(!sctl_payload("--set-detect","0",NULL,NULL,&message,out,&n));assert(message==876 && n==2 && !memcmp(out,"\x08\x00",2));
 assert(!sctl_payload("--set-permissions",NULL,"1","0",&message,out,&n));assert(message==768 && n==6 && !memcmp(out,"\x1a\x04\x18\x01\x20\x00",6));
 assert(!sctl_payload("--set-permissions",NULL,NULL,"1",&message,out,&n));assert(n==4 && !memcmp(out,"\x1a\x02\x20\x01",4));
 assert(!sctl_payload("--set-permissions",NULL,"0",NULL,&message,out,&n));assert(n==4 && !memcmp(out,"\x1a\x02\x18\x00",4));
 for(unsigned i=0;i<5;i++) {
  const char *bad[]={"","2","01","-1","true"};
  assert(sctl_payload("--set-tracking",bad[i],NULL,NULL,&message,out,&n));
  assert(sctl_payload("--set-permissions",NULL,bad[i],"1",&message,out,&n));
 }
 assert(sctl_payload("--set-tracking",NULL,NULL,NULL,&message,out,&n));
 assert(sctl_payload("--set-tracking","1","0",NULL,&message,out,&n));
 assert(sctl_payload("--set-permissions",NULL,NULL,NULL,&message,out,&n));
 assert(sctl_payload("--set-permissions","0","1",NULL,&message,out,&n));
 assert(sctl_payload("--unknown","1",NULL,NULL,&message,out,&n));
 return 0;
}
