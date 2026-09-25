#define _GNU_SOURCE
#include "ssc_journal.h"
#include <assert.h>
#include <stdlib.h>
#include <stdio.h>

static int fail_sync;
int __real_fsync(int);
int __wrap_fsync(int fd) {
    if(fail_sync) { errno=EIO; return -1; }
    return __real_fsync(fd);
}
int main(void) {
    char directory[]="/tmp/hoki-ssc-test-XXXXXX";
    assert(mkdtemp(directory));
    struct ssc_journal j;
    assert(sj_open(&j,directory,1024,0)==0);
    assert(sj_append(&j,3,33,"abc",3)==0);
    assert(sj_finish(&j)==0);
    assert(sj_open(&j,directory,1024,0)<0 && errno==EEXIST);
    sj_abort(&j);
    printf("%s/events.ssc\n",directory);

    char small[]="/tmp/hoki-ssc-small-XXXXXX";
    assert(mkdtemp(small));
    assert(sj_open(&j,small,56,0)==0);
    assert(sj_append(&j,3,33,"abc",3)<0 && errno==EFBIG);
    assert(sj_finish(&j)<0);

    char bad[]="/tmp/hoki-ssc-sync-XXXXXX";
    assert(mkdtemp(bad));
    assert(sj_open(&j,bad,1024,0)==0);
    assert(sj_append(&j,3,33,"abc",3)==0);
    fail_sync=1;
    assert(sj_finish(&j)<0 && errno==EIO);
    assert(j.fd==-1 && j.dir==-1);
    puts("budget, exclusive create and fsync failure checks passed");
}
