#include "minute_tracker.h"
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
static int unhex(const char *s,unsigned char *out,size_t cap) {
    size_t n=strcspn(s,"\r\n");if(n%2 || n/2>cap)return -1;
    for(size_t i=0;i<n/2;i++) {
        unsigned value=0;
        for(unsigned j=0;j<2;j++) {
            unsigned c=(unsigned char)s[i*2+j];
            unsigned digit=c>='0'&&c<='9'?c-'0':c>='a'&&c<='f'?c-'a'+10:99;
            if(digit>15)return -1;
            value=value*16+digit;
        }
        out[i]=(unsigned char)value;
    }
    return (int)(n/2);
}
int main(int argc,char **argv) {
    if(argc!=2)return 64;
    unsigned char suid[18],raw[65536];
    if(unhex(argv[1],suid,sizeof suid)!=18)return 64;
    struct minute_tracker t;
    assert(mt_init(&t,suid,18)==0);
    assert(mt_begin(&t)==0);
    assert(mt_begin(&t)<0 && t.error);
    assert(mt_init(&t,suid,18)==0 && mt_begin(&t)==0);
    char *line=malloc(131075);assert(line);
    while(fgets(line,131075,stdin)) {
        int n=unhex(line,raw,sizeof raw);
        if(n<0 || mt_feed(&t,33,raw,(size_t)n)) { free(line);return 2; }
    }
    free(line);
    printf("pending=%d completed=%u chunks=%u bytes=%llu\n",t.pending,t.completed,t.chunks,(unsigned long long)t.bytes);
    return t.pending?3:0;
}
