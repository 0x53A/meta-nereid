#ifndef SSC_OWNER_H
#define SSC_OWNER_H
#include <errno.h>
#include <fcntl.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <unistd.h>
/* Keep this inode permanently: unlinking a flock file splits ownership. */
static int sowner_acquire(const char *path) {
    int fd=open(path,O_RDWR|O_CREAT|O_CLOEXEC|O_NOFOLLOW|O_NONBLOCK,0600);
    if(fd<0) return -1;
    struct stat st;
    if(fstat(fd,&st)) { int e=errno;close(fd);errno=e;return -1; }
    if(!S_ISREG(st.st_mode) || st.st_uid!=geteuid() || st.st_nlink!=1 ||
       (st.st_mode&077)!=0) { close(fd);errno=EPERM;return -1; }
    if(flock(fd,LOCK_EX|LOCK_NB)) { int e=errno;close(fd);errno=e;return -1; }
    return fd;
}
#endif
