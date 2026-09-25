#define _GNU_SOURCE
#include "ssc_inventory.h"
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
 struct ssc_discovery d={0};unsigned char raw[65536];char *line=malloc(131075);if(!line)return 1;
 while(fgets(line,131075,stdin)){int n=unhex(line,raw,sizeof raw);if(n<0){free(line);return 2;}sd_feed(&d,33,raw,(size_t)n);}
 free(line);
 return sd_publish(&d,argv[1],"12345678-1234-1234-1234-123456789abc","12345678-1234-1234-1234-123456789abd",0)?1:0;
}
