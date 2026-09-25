#ifndef SSC_WIRE_H
#define SSC_WIRE_H
#include <stddef.h>
#include <stdint.h>
struct mt_field { unsigned number,wire; const unsigned char *data; size_t len; uint64_t value; };
static int mt_varint(const unsigned char **p,const unsigned char *end,uint64_t *v) {
    *v=0;
    for(unsigned shift=0;shift<70;shift+=7) {
        if(*p==end)return -1;
        unsigned b=*(*p)++;
        if(shift==63 && b>1)return -1;
        *v|=(uint64_t)(b&127)<<shift;
        if(!(b&128))return 0;
    }
    return -1;
}
static int mt_next(const unsigned char **p,const unsigned char *end,struct mt_field *f) {
    uint64_t key,size;
    if(*p==end)return 0;
    if(mt_varint(p,end,&key) || !(key>>3) || key>>3>0x1fffffff)return -1;
    *f=(struct mt_field){.number=(unsigned)(key>>3),.wire=(unsigned)(key&7)};
    if(f->wire==0)return mt_varint(p,end,&f->value)?-1:1;
    if(f->wire==2) { if(mt_varint(p,end,&size))return -1; }
    else if(f->wire==1)size=8;
    else if(f->wire==5)size=4;
    else return -1;
    if(size>(uint64_t)(end-*p))return -1;
    f->data=*p;f->len=(size_t)size;*p+=size;
    if(f->wire!=2)for(unsigned i=0;i<size;i++)f->value|=(uint64_t)f->data[i]<<(8*i);
    return 1;
}
static int mt_get(const unsigned char *p,size_t len,unsigned number,unsigned wire,
                  struct mt_field *result) {
    const unsigned char *end=p+len;struct mt_field f;int found=0,rc;
    while((rc=mt_next(&p,end,&f))>0)if(f.number==number) {
        if(found || f.wire!=wire)return -1;
        *result=f;found=1;
    }
    return rc<0?-1:found;
}
#endif
