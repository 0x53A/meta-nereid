#define _GNU_SOURCE
#include "ssc_wait.h"
#include <assert.h>
#include <pthread.h>
#include <sys/wait.h>
#include <time.h>
static void *sender(void *arg) {
 sigset_t mask;assert(!pthread_sigmask(SIG_SETMASK,NULL,&mask));
 assert(sigismember(&mask,SIGTERM)&&sigismember(&mask,SIGINT));
 usleep(20000);assert(!kill(getpid(),*(int *)arg));return NULL;
}
static void trial(int signal_number) {
 struct ssc_wait w;assert(!swait_init(&w));
 assert(swait_ms(&w,0)==0);
 struct timespec a,b;assert(!clock_gettime(CLOCK_MONOTONIC,&a));
 assert(swait_ms(&w,20)==0);assert(!clock_gettime(CLOCK_MONOTONIC,&b));
 assert((b.tv_sec-a.tv_sec)*1000000000LL+b.tv_nsec-a.tv_nsec>=15000000);
 pthread_t thread;assert(!pthread_create(&thread,NULL,sender,&signal_number));
 assert(swait_ms(&w,30000)==1);assert(w.stopped==signal_number);
 assert(swait_ms(&w,30000)==1);assert(!pthread_join(thread,NULL));
 close(w.signal_fd);
}
int main(void) {
 for(int i=0;i<2;i++) {
  pid_t child=fork();assert(child>=0);
  if(!child){trial(i?SIGINT:SIGTERM);_exit(0);}
  int status;assert(waitpid(child,&status,0)==child);assert(WIFEXITED(status)&&WEXITSTATUS(status)==0);
 }
 return 0;
}
