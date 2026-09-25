#define _GNU_SOURCE
#include "ssc_config.h"
#include <assert.h>
#include <stdlib.h>
int __real_fsync(int);
int __wrap_fsync(int fd) { if(getenv("TEST_FSYNC_FAIL")){errno=EIO;return -1;}return __real_fsync(fd); }
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
 if(argc!=2 && argc!=3)return 64;
 const char *mode=argc==3?argv[2]:"--tracking-config";
 unsigned event=!strcmp(mode,"--chrm-config")?775:!strcmp(mode,"--tracker-config")?1029:776;
 const unsigned char source[18]={9,0,1,2,3,4,5,6,7,17,8,9,10,11,12,13,14,15};
 struct ssc_config c;assert(!sc_init(&c,source,event));
 unsigned char raw[65536];char *line=malloc(131075);assert(line);
 while(fgets(line,131075,stdin)){int n=unhex(line,raw,sizeof raw);if(n<0){free(line);return 2;}sc_feed(&c,33,raw,(size_t)n);}
 free(line);
 int result=sc_publish(&c,argv[1],"12345678-1234-1234-1234-123456789abc","12345678-1234-1234-1234-123456789abd",mode,source);
 if(!result) {
  if(c.size)c.payload[0]^=1;
  assert(sc_publish(&c,argv[1],"12345678-1234-1234-1234-123456789abc","12345678-1234-1234-1234-123456789abd",mode,source)<0 && errno==EEXIST);
 }
 return result?1:0;
}
