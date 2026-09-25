#ifndef SSC_CONTROLS_H
#define SSC_CONTROLS_H
#include <stddef.h>
#include <string.h>
/* Only canonical boolean values; NULL is absent, never implicitly false. */
static int sctl_flag(const char *text) {
 if(!text)return -1;
 if(!strcmp(text,"0"))return 0;
 if(!strcmp(text,"1"))return 1;
 return -2;
}
static int sctl_payload(const char *mode,const char *value,const char *rhr,const char *sleep,
                        unsigned *message,unsigned char out[8],size_t *size) {
 *size=0;
 if(!strcmp(mode,"--set-tracking") || !strcmp(mode,"--set-detect")) {
  int v=sctl_flag(value);if(v<0 || rhr || sleep)return -1;
  *message=!strcmp(mode,"--set-tracking")?776:876;
  out[0]=8;out[1]=(unsigned char)v;*size=2;return 0;
 }
 if(!strcmp(mode,"--set-permissions")) {
  int a=sctl_flag(rhr),b=sctl_flag(sleep);
  if(value || a==-2 || b==-2 || (a<0 && b<0))return -1;
  *message=768;out[0]=26;size_t n=2;
  if(a>=0){out[n++]=24;out[n++]=(unsigned char)a;}
  if(b>=0){out[n++]=32;out[n++]=(unsigned char)b;}
  out[1]=(unsigned char)(n-2);*size=n;return 0;
 }
 return -1;
}
#endif
