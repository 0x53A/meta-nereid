#ifndef HOKI_SSC_LIMITS_H
#define HOKI_SSC_LIMITS_H
#include <errno.h>
#include <stdint.h>
#include <stdlib.h>
/* All modes retain a fixed256MiB free-space reserve. Never delete to make room. */
static int sl_budget(const char *text,int continuous,uint64_t *limit) {
 if(!limit || (continuous!=0 && continuous!=1))return EINVAL;
 if(!text) {*limit=(continuous?128ULL:16ULL)*1024*1024;return 0;}
 if(!*text)return EINVAL;
 for(const char *p=text;*p;p++)if(*p<'0'||*p>'9')return EINVAL;
 errno=0;char *end;unsigned long long value=strtoull(text,&end,10);
 if(errno || *end || value<16ULL*1024*1024 || value>1024ULL*1024*1024)return EINVAL;
 *limit=(uint64_t)value;return 0;
}
#endif
