#ifndef SSC_EXTRA_READS_H
#define SSC_EXTRA_READS_H
#include <stddef.h>
#include <string.h>
/* Stock-code-derived diagnostic getters (task0315), not measurement activation.
 * Firmware acceptance/absence of side effects still requires live validation. */
static int ser_plan(const char *mode,unsigned *request,unsigned *reply,
                    unsigned char payload[8],size_t *size) {
 *size=0;
 if(!strcmp(mode,"--chrm-config")) {
  *request=775;*reply=775;return 0;
 }
 if(!strcmp(mode,"--tracker-config")) {
  /* READ_TRACKER=1, INVALID state=0, explicit empty required payload. */
  const unsigned char read[]={8,1,16,0,26,0};
  *request=768;*reply=1029;*size=sizeof read;
  memcpy(payload,read,sizeof read);return 0;
 }
 return -1;
}
#endif
