#ifndef SSC_WAIT_H
#define SSC_WAIT_H
#include <errno.h>
#include <poll.h>
#include <signal.h>
#include <stdint.h>
#include <sys/signalfd.h>
#include <sys/timerfd.h>
#include <unistd.h>
/* Initialize before any worker/vendor thread: all inherit the blocked mask. */
struct ssc_wait { int signal_fd, stopped, error; };
static int swait_init(struct ssc_wait *w) {
 *w=(struct ssc_wait){.signal_fd=-1};
 sigset_t mask;sigemptyset(&mask);sigaddset(&mask,SIGTERM);sigaddset(&mask,SIGINT);
 if(sigprocmask(SIG_BLOCK,&mask,NULL))return -1;
 w->signal_fd=signalfd(-1,&mask,SFD_CLOEXEC|SFD_NONBLOCK);
 return w->signal_fd<0?-1:0;
}
/* No alarm wake: shared HAL/RTC policy supplies suspend wakeups. */
static int swait_ms(struct ssc_wait *w,unsigned ms) {
 if(w->error)return -1;
 if(w->stopped)return 1;
 int timer=-1;
 if(ms) {
  timer=timerfd_create(CLOCK_BOOTTIME,TFD_CLOEXEC|TFD_NONBLOCK);
  if(timer<0){w->error=errno;return -1;}
  struct itimerspec spec={.it_value={.tv_sec=ms/1000,.tv_nsec=(ms%1000)*1000000L}};
  if(timerfd_settime(timer,0,&spec,NULL)){w->error=errno;close(timer);return -1;}
 }
 struct pollfd fds[2]={{.fd=w->signal_fd,.events=POLLIN},{.fd=timer,.events=POLLIN}};
 int rc;
 do {rc=poll(fds,2,ms?-1:0);}while(rc<0&&errno==EINTR);
 if(rc<0)w->error=errno;
 else if((fds[0].revents|fds[1].revents)&(POLLERR|POLLHUP|POLLNVAL))w->error=EIO;
 else if(fds[0].revents&POLLIN) {
  struct signalfd_siginfo info;
  if(read(w->signal_fd,&info,sizeof info)!=(ssize_t)sizeof info)w->error=EIO;
  else w->stopped=(int)info.ssi_signo;
 }
 if(timer>=0)close(timer);
 return w->error?-1:w->stopped?1:0;
}
#endif
