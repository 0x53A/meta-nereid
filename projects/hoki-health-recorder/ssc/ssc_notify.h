#ifndef SSC_NOTIFY_H
#define SSC_NOTIFY_H
#include <errno.h>
#include <stddef.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>
static int snotify_ready(const char *endpoint) {
 if(!endpoint)return 0;
 struct sockaddr_un address={.sun_family=AF_UNIX};
 size_t n=strlen(endpoint);
 if(n<2 || (endpoint[0]!='/' && endpoint[0]!='@') ||
    n>sizeof address.sun_path || (endpoint[0]=='/' && n==sizeof address.sun_path)) {
  errno=EINVAL;return -1;
 }
 memcpy(address.sun_path,endpoint,n);
 if(endpoint[0]=='@')address.sun_path[0]=0;
 socklen_t length=(socklen_t)(offsetof(struct sockaddr_un,sun_path)+n+(endpoint[0]=='/'));
 int fd=socket(AF_UNIX,SOCK_DGRAM|SOCK_CLOEXEC|SOCK_NONBLOCK,0);
 if(fd<0)return -1;
 const char message[]="READY=1\nSTATUS=SSC first minute transfer archived; freshness unverified";
 ssize_t sent=sendto(fd,message,sizeof message-1,MSG_NOSIGNAL,(struct sockaddr *)&address,length);
 int error=sent==(ssize_t)sizeof message-1?0:sent<0?errno:EIO;
 close(fd);if(error){errno=error;return -1;}return 0;
}
#endif
