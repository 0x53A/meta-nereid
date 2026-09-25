#include <assert.h>
#include <stdio.h>
#include "ssc_limits.h"
int main(void) {
 uint64_t n=0;
 assert(!sl_budget(NULL,0,&n)&&n==16777216);
 assert(!sl_budget(NULL,1,&n)&&n==134217728);
 assert(!sl_budget("16777216",1,&n)&&n==16777216);
 assert(!sl_budget("1073741824",1,&n)&&n==1073741824);
 const char *bad[]={"","0","16777215","1073741825","-16777216","+16777216"," 16777216","16777216 ","1e9","18446744073709551616"};
 for(unsigned i=0;i<sizeof bad/sizeof *bad;i++)assert(sl_budget(bad[i],1,&n));
 assert(sl_budget(NULL,2,&n));assert(sl_budget(NULL,0,NULL));
 puts("storage budget defaults, bounds, malformed inputs and overflow passed");
 return 0;
}
