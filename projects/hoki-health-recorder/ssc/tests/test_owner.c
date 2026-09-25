#define _GNU_SOURCE
#include "ssc_owner.h"
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
int main(void) {
    char directory[]="/tmp/hoki-ssc-owner-XXXXXX",path[256],alias[256];
    assert(mkdtemp(directory));
    snprintf(path,sizeof path,"%s/owner",directory);
    snprintf(alias,sizeof alias,"%s/alias",directory);
    int fd=sowner_acquire(path);assert(fd>=0);
    pid_t pid=fork();assert(pid>=0);
    if(pid==0) {
        close(fd);
        assert(sowner_acquire(path)<0 && (errno==EWOULDBLOCK || errno==EAGAIN));
        _exit(0);
    }
    int status;assert(waitpid(pid,&status,0)==pid && status==0);
    close(fd);
    fd=sowner_acquire(path);assert(fd>=0);close(fd);
    assert(symlink(path,alias)==0);
    assert(sowner_acquire(alias)<0);
    assert(chmod(path,0666)==0);
    assert(sowner_acquire(path)<0 && errno==EPERM);
    assert(chmod(path,0600)==0);
    assert(unlink(alias)==0 && link(path,alias)==0);
    assert(sowner_acquire(path)<0 && errno==EPERM);
    puts("exclusive ownership, reacquisition and unsafe inode checks passed");
}
