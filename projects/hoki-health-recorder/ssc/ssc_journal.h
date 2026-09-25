#ifndef SSC_JOURNAL_H
#define SSC_JOURNAL_H
/* Bounded research SSC archive. Caller serializes access and holds CPU awake.
 * All integers are little endian; never overwrites an existing capture.
 * Successful finish proves fsync, not sensor/protocol completeness.
 */
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <string.h>
#include <sys/statvfs.h>
#include <time.h>
#include <unistd.h>

struct ssc_journal { int fd, dir, error; uint64_t bytes, seq, limit, reserve; };
static void sj_le(unsigned char *p, uint64_t v, unsigned n) {
    for (unsigned i=0; i<n; ++i) { p[i]=(unsigned char)v; v>>=8; }
}
static uint32_t sj_crc(uint32_t c, const unsigned char *p, size_t n) {
    while(n--) { c^=*p++; for(unsigned i=0;i<8;i++) c=(c>>1)^((0u-(c&1))&0xedb88320u); }
    return c;
}
static int sj_fail(struct ssc_journal *j, int e) {
    if(!j->error) j->error=e?e:EIO;
    errno=j->error; return -1;
}
static int sj_write(struct ssc_journal *j, const void *data, size_t len) {
    const unsigned char *p=data;
    while(len) {
        ssize_t n=write(j->fd,p,len);
        if(n<0 && errno==EINTR) continue;
        if(n<=0) return sj_fail(j,n<0?errno:EIO);
        p+=n; len-=(size_t)n; j->bytes+=(size_t)n;
    }
    return 0;
}
static int sj_space(struct ssc_journal *j, uint64_t need) {
    struct statvfs s;
    if(fstatvfs(j->fd,&s)) return sj_fail(j,errno);
    if(!s.f_frsize) return sj_fail(j,EIO);
    uint64_t units=j->reserve/s.f_frsize+(j->reserve%s.f_frsize!=0);
    uint64_t more=need/s.f_frsize+(need%s.f_frsize!=0);
    if(units>UINT64_MAX-more || (uint64_t)s.f_bavail<units+more)
        return sj_fail(j,ENOSPC);
    return 0;
}
static int sj_open(struct ssc_journal *j, const char *directory,
                   uint64_t limit, uint64_t reserve) {
    *j=(struct ssc_journal){.fd=-1,.dir=-1,.limit=limit,.reserve=reserve};
    if(limit<56) return sj_fail(j,EINVAL);
    j->dir=open(directory,O_RDONLY|O_DIRECTORY|O_CLOEXEC|O_NOFOLLOW);
    if(j->dir<0) return sj_fail(j,errno);
    j->fd=openat(j->dir,"events.ssc",O_WRONLY|O_CREAT|O_EXCL|O_CLOEXEC|O_NOFOLLOW,0600);
    if(j->fd<0) return sj_fail(j,errno);
    const unsigned char header[16]={'H','O','K','I','S','S','C','1',16,0,0,0,40,0,0,0};
    if(sj_space(j,56) || sj_write(j,header,sizeof header)) return -1;
    if(fsync(j->fd) || fsync(j->dir)) return sj_fail(j,errno);
    return 0;
}
/* kind1=request,2=response,3=indication,4=session metadata; 255 reserved. */
static int sj_append_at(struct ssc_journal *j, unsigned kind, unsigned id,
                     const void *payload, size_t len, uint64_t boot_ns, uint64_t real_ns) {
    if(j->error) return sj_fail(j,j->error);
    if(j->fd<0 || kind<1 || (kind>4 && kind!=255) || len>65536 || (!payload&&len))
        return sj_fail(j,EINVAL);
    uint64_t size=40+len, margin=kind==255?0:40;
    if(j->bytes>j->limit || size+margin>j->limit-j->bytes) return sj_fail(j,EFBIG);
    if(sj_space(j,size+margin)) return -1;
    unsigned char h[40]={0};
    sj_le(h,len,4); sj_le(h+4,kind,4); sj_le(h+8,id,4);
    sj_le(h+12,j->seq,8);
    sj_le(h+20,boot_ns,8);
    sj_le(h+28,real_ns,8);
    uint32_t crc=sj_crc(sj_crc(UINT32_MAX,h,36),payload,len)^UINT32_MAX;
    sj_le(h+36,crc,4);
    if(sj_write(j,h,sizeof h) || sj_write(j,payload,len)) return -1;
    ++j->seq;
    return 0;
}
static int sj_append(struct ssc_journal *j, unsigned kind, unsigned id,
                     const void *payload, size_t len) {
    struct timespec boot,real;
    if(clock_gettime(CLOCK_BOOTTIME,&boot) || clock_gettime(CLOCK_REALTIME,&real))
        return sj_fail(j,errno);
    return sj_append_at(j,kind,id,payload,len,
        (uint64_t)boot.tv_sec*1000000000+boot.tv_nsec,
        (uint64_t)real.tv_sec*1000000000+real.tv_nsec);
}
/* Call only after all producers stop. Footer marks orderly acquisition end,
 * never EOP, no-loss, or successful sensor setup. Caller checks those separately.
 */
static int sj_finish(struct ssc_journal *j) {
    int rc=sj_append(j,255,0,NULL,0);
    if(!rc && fsync(j->fd)) rc=sj_fail(j,errno);
    if(j->fd>=0 && close(j->fd) && !rc) rc=sj_fail(j,errno);
    j->fd=-1;
    if(j->dir>=0 && close(j->dir) && !rc) rc=sj_fail(j,errno);
    j->dir=-1;
    return rc;
}
static void sj_abort(struct ssc_journal *j) {
    if(j->fd>=0) close(j->fd);
    if(j->dir>=0) close(j->dir);
    j->fd=j->dir=-1;
}
#endif
