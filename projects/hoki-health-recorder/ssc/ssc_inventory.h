#ifndef SSC_INVENTORY_H
#define SSC_INVENTORY_H
#include "ssc_discovery.h"
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <unistd.h>
/* Called only after callback quiescence and successful raw archive finalization. */
static int sd_publish(const struct ssc_discovery *d,const char *directory,const char *boot,const char *session,int interrupted) {
 int dir=open(directory,O_RDONLY|O_DIRECTORY|O_CLOEXEC|O_NOFOLLOW);if(dir<0)return -1;
 int fd=openat(dir,"inventory.pending",O_WRONLY|O_CREAT|O_EXCL|O_CLOEXEC|O_NOFOLLOW,0600);
 if(fd<0){close(dir);return -1;}
 FILE *f=fdopen(fd,"w");if(!f){close(fd);close(dir);return -1;}
 fprintf(f,"{\"version\":1,\"boot_id\":\"%s\",\"session_id\":\"%s\",\"scope\":\"queried SSC datatypes only\",\"parse_error\":%s,\"interrupted\":%s,\"streams\":[",boot,session,d->error?"true":"false",interrupted?"true":"false");
 for(unsigned i=0;i<SD_COUNT;i++) {
  const struct sd_entry *e=&d->entries[i];
  const char *status=!e->responses?"no_response":!e->count?"empty":e->count==1?"unique":"ambiguous";
  fprintf(f,"%s{\"data_type\":\"%s\",\"status\":\"%s\",\"responses\":%u,\"suids\":[",i?",":"",sd_names[i],status,e->responses);
  for(unsigned j=0;j<e->count;j++) {
   fprintf(f,"%s\"",j?",":"");for(unsigned k=0;k<18;k++)fprintf(f,"%02x",e->suids[j][k]);fputc('"',f);
  }
  fputs("]}",f);
 }
 fputs("]}\n",f);
 int error=ferror(f)?EIO:0;
 if(fflush(f) && !error)error=errno;
 if(!error && fsync(fd))error=errno;
 if(fclose(f) && !error)error=errno;
 if(!error && linkat(dir,"inventory.pending",dir,"inventory.json",0))error=errno;
 if(unlinkat(dir,"inventory.pending",0) && !error)error=errno;
 if(!error && fsync(dir))error=errno;
 close(dir);if(error){errno=error;return -1;}return 0;
}
#endif
