#define _GNU_SOURCE
#include "ssc_notify.h"
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
int main(void) {
 assert(!snotify_ready(NULL));
 assert(snotify_ready("")<0 && errno==EINVAL);
 assert(snotify_ready("relative")<0 && errno==EINVAL);
 assert(snotify_ready("@")<0 && errno==EINVAL);
 for(int abstract=0;abstract<2;abstract++) {
  char endpoint[100];snprintf(endpoint,sizeof endpoint,abstract?"@hoki-notify-%ld":"/tmp/hoki-notify-%ld",(long)getpid());
  int fd=socket(AF_UNIX,SOCK_DGRAM|SOCK_CLOEXEC,0);assert(fd>=0);
  struct timeval timeout={.tv_sec=1};assert(!setsockopt(fd,SOL_SOCKET,SO_RCVTIMEO,&timeout,sizeof timeout));
  struct sockaddr_un addr={.sun_family=AF_UNIX};size_t n=strlen(endpoint);memcpy(addr.sun_path,endpoint,n);
  if(abstract)addr.sun_path[0]=0;
  assert(!bind(fd,(struct sockaddr *)&addr,offsetof(struct sockaddr_un,sun_path)+n+!abstract));
  assert(!snotify_ready(endpoint));char data[256];ssize_t count=recv(fd,data,sizeof data,0);
  assert(count>0 && !memcmp(data,"READY=1\nSTATUS=",15));
  close(fd);assert(snotify_ready(endpoint)<0);
  if(!abstract)assert(!unlink(endpoint));
 }
 return 0;
}
