#define _GNU_SOURCE
#include "ssc_status.h"
#include <assert.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>

static int fail_sync;
int __real_fsync(int);
int __wrap_fsync(int fd) {
 if(fail_sync) { errno=EIO;return -1; }
 return __real_fsync(fd);
}
static void read_named(int directory,const char *name,char out[640]) {
 int fd=openat(directory,name,O_RDONLY|O_CLOEXEC);assert(fd>=0);
 ssize_t n=read(fd,out,639);assert(n>0 && n<639);out[n]=0;assert(!close(fd));
}
static void read_status(int directory,char out[640]) { read_named(directory,"status.json",out); }
int main(void) {
 char dir[]="/tmp/hoki-ssc-status-XXXXXX";assert(mkdtemp(dir));
 int d=open(dir,O_RDONLY|O_DIRECTORY|O_CLOEXEC);assert(d>=0);
 struct ssc_status s;assert(!ss_open(&s,d,64));char before[640],after[640];
 read_status(d,before);assert(strstr(before,"\"phase\":\"started\""));
 assert(strstr(before,"\"archive_complete\":false"));
 /* A second owner cannot replace the existing status. */
 struct ssc_status other;assert(ss_open(&other,d,64)==EEXIST);ss_close(&other);
 read_status(d,after);assert(!strcmp(before,after));
 /* Progress has a separate namespace and cannot masquerade as final status. */
 assert(!ss_progress(&s,7,5,0,3,64));
 char progress[640];read_named(d,"progress.json",progress);
 assert(strstr(progress,"\"phase\":\"recording\""));
 assert(strstr(progress,"\"archive_complete\":false"));
 assert(strstr(progress,"\"durable\":5"));
 assert(strstr(progress,"\"publication_boottime_ns\":"));
 read_status(d,after);assert(!strcmp(before,after));
 fail_sync=1;assert(ss_progress(&s,8,8,0,3,64)==EIO);fail_sync=0;
 read_named(d,"progress.json",after);assert(!strcmp(progress,after));
 assert(!ss_progress(&s,9,9,0,3,64));
 read_named(d,"progress.json",after);assert(strstr(after,"\"durable\":9"));
 /* Failed fsync leaves the last published snapshot intact. */
 fail_sync=1;assert(ss_publish(&s,1,0,4,4,0,3,64)==EIO);fail_sync=0;
 read_status(d,after);assert(!strcmp(before,after));
 assert(!ss_publish(&s,1,ENOBUFS,65,1,1,64,64));
 read_status(d,after);assert(strstr(after,"\"archive_complete\":false"));
 assert(strstr(after,"\"accepted_not_confirmed_durable\":64"));
 ss_close(&s);close(d);
 /* Abrupt process exit retains the explicit started phase. */
 char crash[]="/tmp/hoki-ssc-status-crash-XXXXXX";assert(mkdtemp(crash));
 d=open(crash,O_RDONLY|O_DIRECTORY|O_CLOEXEC);assert(d>=0);
 pid_t pid=fork();assert(pid>=0);
 if(!pid) { struct ssc_status child;_exit(ss_open(&child,d,64)?1:0); }
 int result;assert(waitpid(pid,&result,0)==pid && WIFEXITED(result) && WEXITSTATUS(result)==0);
 read_status(d,after);assert(strstr(after,"\"phase\":\"started\""));close(d);
 puts("exclusive creation, atomic failure preservation, loss counts and abrupt exit passed");
 return 0;
}
