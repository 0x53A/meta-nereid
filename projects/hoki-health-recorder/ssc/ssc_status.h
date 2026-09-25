#ifndef HOKI_SSC_STATUS_H
#define HOKI_SSC_STATUS_H
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <stdio.h>
#include <unistd.h>
#include <time.h>

struct ssc_status { int directory, initialized, progress_initialized; };
/* Caller serializes status writes, owns capture directory, and holds CPU awake.
 * A started record is only a startup snapshot, not live record counts.
 * Complete means archive finalization, never sensor/protocol completeness.
 * On failure, caller must propagate errno even if a rename became visible. */
static int ss_publish_internal(struct ssc_status *s,int progress,int closed,int archive_error,
                      uint64_t accepted,uint64_t durable,uint64_t rejected,
                      unsigned high_water,unsigned capacity) {
 char data[768];
 const char *pending=progress?"progress.pending":"status.pending";
 const char *published=progress?"progress.json":"status.json";
 int *initialized=progress?&s->progress_initialized:&s->initialized;
 struct timespec stamp;
 if(clock_gettime(CLOCK_BOOTTIME,&stamp))return errno;
 uint64_t publication=(uint64_t)stamp.tv_sec*1000000000+stamp.tv_nsec;
 if((progress && closed) || s->directory<0 || durable>accepted || high_water>capacity || archive_error<0 ||
    (closed && !archive_error && (rejected || accepted!=durable))) return EINVAL;
 int n=snprintf(data,sizeof data,
  "{\"version\":1,\"phase\":\"%s\",\"archive_complete\":%s,"
  "\"archive_error\":%d,\"accepted\":%" PRIu64 ",\"durable\":%" PRIu64
  ",\"rejected\":%" PRIu64 ",\"accepted_not_confirmed_durable\":%" PRIu64
  ",\"queue_high_water\":%u,\"queue_capacity\":%u,\"protocol_complete\":null,\"publication_boottime_ns\":%" PRIu64 "}\n",
  progress?"recording":(closed?"closed":"started"),closed&&!archive_error?"true":"false",archive_error,
  accepted,durable,rejected,accepted-durable,high_water,capacity,publication);
 if(n<0 || (size_t)n>=sizeof data)return EOVERFLOW;
 int fd=openat(s->directory,pending,O_WRONLY|O_CREAT|O_EXCL|O_CLOEXEC|O_NOFOLLOW,0600);
 if(fd<0)return errno;
 int error=0;size_t offset=0;
 while(offset<(size_t)n) {
  ssize_t written=write(fd,data+offset,(size_t)n-offset);
  if(written<0 && errno==EINTR)continue;
  if(written<=0) { error=written<0?errno:EIO;break; }
  offset+=(size_t)written;
 }
 if(!error && fsync(fd))error=errno;
 if(close(fd) && !error)error=errno;
 if(!error) {
  if(*initialized) {
   if(renameat(s->directory,pending,s->directory,published))error=errno;
  } else {
   /* Initial publish cannot replace somebody else's status. */
   if(linkat(s->directory,pending,s->directory,published,0))error=errno;
   else *initialized=1;
  }
 }
 if(unlinkat(s->directory,pending,0) && errno!=ENOENT && !error)error=errno;
 if(!error && fsync(s->directory))error=errno;
 return error;
}
static int ss_publish(struct ssc_status *s,int closed,int archive_error,
                      uint64_t accepted,uint64_t durable,uint64_t rejected,
                      unsigned high_water,unsigned capacity) {
 return ss_publish_internal(s,0,closed,archive_error,accepted,durable,rejected,high_water,capacity);
}
static inline int ss_progress(struct ssc_status *s,uint64_t accepted,uint64_t durable,
                              uint64_t rejected,unsigned high_water,unsigned capacity) {
 return ss_publish_internal(s,1,0,0,accepted,durable,rejected,high_water,capacity);
}
static int ss_open(struct ssc_status *s,int directory,unsigned capacity) {
 *s=(struct ssc_status){.directory=-1};
 s->directory=fcntl(directory,F_DUPFD_CLOEXEC,3);
 if(s->directory<0)return errno;
 return ss_publish(s,0,0,0,0,0,0,capacity);
}
static void ss_close(struct ssc_status *s) {
 if(s->directory>=0)close(s->directory);
 s->directory=-1;
}
#endif
