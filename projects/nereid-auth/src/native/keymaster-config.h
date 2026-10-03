/* Explicit persistent vendor-version configuration. Never synthesize values
 * from the Nereid/Linux release or substitute zeros. Author: Lukas Rieger <code@lukasrieger.com>. */
#ifndef NEREID_KEYMASTER_CONFIG_H
#define NEREID_KEYMASTER_CONFIG_H
#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>
#include <errno.h>
#include <stdint.h>
#include <stddef.h>
#include <string.h>

static int km_parse_config(const unsigned char *data,size_t length,uint32_t out[3])
{
    size_t pos=0;
    for(unsigned i=0;i<3;i++) {
        uint32_t v=0; unsigned digits=0;
        while(pos<length && data[pos]>='0' && data[pos]<='9') {
            unsigned digit=data[pos++]-'0';
            if(v>(UINT32_MAX-digit)/10 || ++digits>8) return -1;
            v=v*10+digit;
        }
        if(!digits || pos==length || data[pos++]!='\n') return -1;
        out[i]=v;
    }
    if(pos!=length || out[0]==0 || out[0]>999999 ||
       out[1]<201501 || out[1]>209912 || out[1]%100<1 || out[1]%100>12 ||
       out[2]<20150101 || out[2]>20991231 ||
       out[2]%100<1 || out[2]%100>31 || (out[2]/100)%100<1 || (out[2]/100)%100>12) return -1;
    return 0;
}
static int km_read_config(uint32_t out[3])
{
    int fd=open("/var/lib/nereid-auth/keymaster.conf",O_RDONLY|O_CLOEXEC|O_NOFOLLOW);
    if(fd<0) return -1;
    unsigned char text[64]={0}; size_t length=0; struct stat st;
    int rc=-1;
    if(fstat(fd,&st) || !S_ISREG(st.st_mode) || st.st_uid!=0 ||
       (st.st_mode&0777)!=0600 || st.st_size<=0 || st.st_size>=(off_t)sizeof(text)) goto out;
    while(length<sizeof(text)) {
        ssize_t n=read(fd,text+length,sizeof(text)-length);
        if(n<0 && errno==EINTR) continue;
        if(n<0) goto out;
        if(!n) break;
        length+=(size_t)n;
    }
    rc=km_parse_config(text,length,out);
out:
    close(fd); return rc;
}
#endif
