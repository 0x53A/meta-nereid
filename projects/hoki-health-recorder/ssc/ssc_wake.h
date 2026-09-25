#ifndef SSC_WAKE_H
#define SSC_WAKE_H
#include <errno.h>
#include <fcntl.h>
#include <unistd.h>
struct ssc_wake { int lock,unlock,held,maybe_held,error; };
static int swake_write(int fd) {
    const char name[]="hoki_ssc_recording\n";
    ssize_t n=write(fd,name,sizeof name-1);
    return n==(ssize_t)sizeof name-1?0:(n<0?errno:EIO);
}
static int swake_hold(void *ctx) {
    struct ssc_wake *g=ctx;
    if(g->error) return g->error;
    if(g->held) return 0;
    g->maybe_held=1;
    int rc=swake_write(g->lock);
    if(rc) return g->error=rc;
    g->held=1;return 0;
}
static int swake_release(void *ctx) {
    struct ssc_wake *g=ctx;
    if(!g->maybe_held) return g->error;
    int rc=swake_write(g->unlock);
    if(rc) return g->error=rc;
    g->held=g->maybe_held=0;
    return g->error;
}
static int swake_open(struct ssc_wake *g) {
    *g=(struct ssc_wake){.lock=-1,.unlock=-1};
    g->lock=open("/sys/power/wake_lock",O_WRONLY|O_CLOEXEC);
    if(g->lock<0) return g->error=errno;
    g->unlock=open("/sys/power/wake_unlock",O_WRONLY|O_CLOEXEC);
    if(g->unlock<0) return g->error=errno;
    return 0;
}
static void swake_close(struct ssc_wake *g) {
    swake_release(g);
    if(g->lock>=0) close(g->lock);
    if(g->unlock>=0) close(g->unlock);
}
#endif
