/* Narrow captured Qualcomm Keymaster AES-GCM wire contract.
 * Author: Lukas Rieger <code@lukasrieger.com>
 * Only normal TEE operations; no import, export, upgrade, reset or provisioning.
 */
#ifndef NEREID_KEYMASTER_WIRE_H
#define NEREID_KEYMASTER_WIRE_H
#include <stdint.h>
#include <stddef.h>
#include <string.h>
#include <limits.h>

#define KM_KEY_MAX 4096U
#define KM_WIRE_MAX 8192U
#define KM_HAT_SIZE 69U
#define KM_RECORD_HEADER 76U
#define KM_RECORD_MAX (KM_RECORD_HEADER + KM_KEY_MAX)
#define KM_PURPOSE 0x20000001U
#define KM_ALGORITHM 0x10000002U
#define KM_KEY_SIZE 0x30000003U
#define KM_BLOCK_MODE 0x20000004U
#define KM_PADDING 0x20000006U
#define KM_MIN_MAC 0x30000008U
#define KM_SID 0xa00001f6U
#define KM_AUTH_TYPE 0x100001f8U
#define KM_NONCE 0x900003e9U
#define KM_AUTH_TOKEN 0x900003eaU
#define KM_MAC_LENGTH 0x300003ebU

struct km_blob { unsigned char data[KM_KEY_MAX]; size_t length; };
struct km_wire { unsigned char data[KM_WIRE_MAX]; size_t used; };
struct km_param { uint32_t tag; uint64_t value; const unsigned char *bytes; size_t length; };
typedef int (*km_send_fn)(void *, unsigned char *, size_t, size_t);
struct km_context {
    unsigned char *shared;
    size_t capacity;
    km_send_fn send;
    void *opaque;
    int broken;
    int32_t status;
};
static uint32_t km_u32(const unsigned char *p)
{ return (uint32_t)p[0]|((uint32_t)p[1]<<8)|((uint32_t)p[2]<<16)|((uint32_t)p[3]<<24); }
static uint64_t km_u64(const unsigned char *p)
{ return (uint64_t)km_u32(p)|((uint64_t)km_u32(p+4)<<32); }
static void km_put32(unsigned char *p,uint32_t v)
{ p[0]=(unsigned char)v; p[1]=(unsigned char)(v>>8); p[2]=(unsigned char)(v>>16); p[3]=(unsigned char)(v>>24); }
static void km_put64(unsigned char *p,uint64_t v)
{ km_put32(p,(uint32_t)v); km_put32(p+4,(uint32_t)(v>>32)); }
static void km_clear(void *p,size_t n)
{ volatile unsigned char *b=p; while(n--) *b++=0; }
static int km_append(struct km_wire *w,const unsigned char *data,size_t n)
{
    if(n>sizeof(w->data)-w->used || (n && !data)) return -1;
    if(n) memcpy(w->data+w->used,data,n);
    w->used+=n; return 0;
}
/* Vendor parameters are 12-byte records. Blob offsets are relative to the
 * complete command, with their data following the entire reserved record array.
 * The record's scalar union is explicitly zero-filled before writing its value. */
static int km_params(struct km_wire *w,const struct km_param *p,size_t count)
{
    if(count>16 || count>(sizeof(w->data)-w->used)/12) return -1;
    size_t start=w->used;
    memset(w->data+start,0,count*12); w->used+=count*12;
    for(size_t i=0;i<count;i++) {
        unsigned char *r=w->data+start+i*12;
        km_put32(r,p[i].tag);
        unsigned type=p[i].tag>>28;
        if(type==9) {
            if(p[i].length>UINT32_MAX) return -1;
            km_put32(r+4,(uint32_t)w->used); km_put32(r+8,(uint32_t)p[i].length);
            if(km_append(w,p[i].bytes,p[i].length)) return -1;
        } else if(type==1 || type==2 || type==3 || type==4) {
            if(p[i].value>UINT32_MAX) return -1;
            km_put32(r+4,(uint32_t)p[i].value);
        } else if(type==5 || type==6 || type==10) {
            km_put64(r+4,p[i].value);
        } else return -1;
    }
    return 0;
}
static int km_exchange(struct km_context *k,const struct km_wire *w,
                       const unsigned char **response,size_t *capacity)
{
    if(k->broken || w->used>k->capacity || k->capacity-w->used<32) return -1;
    memset(k->shared,0,k->capacity);
    memcpy(k->shared,w->data,w->used);
    km_put32(k->shared+w->used,0x80000000U);
    if(k->send(k->opaque,k->shared,w->used,k->capacity-w->used)) {
        k->broken=1; return -1;
    }
    *response=k->shared+w->used; *capacity=k->capacity-w->used;
    uint32_t raw=km_u32(*response);
    k->status=raw<=INT32_MAX ? (int32_t)raw : -1-(int32_t)(UINT32_MAX-raw);
    if(k->status==INT32_MIN) { k->broken=1; return -1; }
    return k->status ? -1 : 0;
}
static int km_range(size_t capacity,uint32_t offset,size_t length,size_t header)
{ return offset>=header && offset<=capacity && length<=capacity-offset; }
static int km_configure(struct km_context *k,const uint32_t versions[3])
{
    struct km_wire w={.used=12};
    const struct km_param params[]={
        {0x300002c1U,versions[0],NULL,0},
        {0x300002c2U,versions[1],NULL,0},
        {0x300002ceU,versions[2],NULL,0}
    };
    const unsigned char *r=NULL; size_t cap=0;
    km_put32(w.data,0x116); km_put32(w.data+4,12); km_put32(w.data+8,3);
    int rc=km_params(&w,params,3);
    if(!rc) rc=km_exchange(k,&w,&r,&cap);
    km_clear(&w,sizeof(w)); km_clear(k->shared,k->capacity); return rc;
}
static int km_generate(struct km_context *k,uint64_t sid,struct km_blob *blob)
{
    struct km_wire w={.used=12};
    const struct km_param params[]={
        {KM_PURPOSE,0,NULL,0},{KM_PURPOSE,1,NULL,0},
        {KM_ALGORITHM,32,NULL,0},{KM_KEY_SIZE,256,NULL,0},
        {KM_BLOCK_MODE,32,NULL,0},{KM_PADDING,1,NULL,0},
        {KM_MIN_MAC,128,NULL,0},{KM_SID,sid,NULL,0},{KM_AUTH_TYPE,1,NULL,0}
    };
    int rc=-1; const unsigned char *r=NULL; size_t cap=0;
    if(!sid) goto out;
    km_put32(w.data,0x108); km_put32(w.data+4,12); km_put32(w.data+8,9);
    if(km_params(&w,params,9) || km_exchange(k,&w,&r,&cap)) goto out;
    /* generate_key_common at 0x90c4..0x90e8: blob offset +8, length +12. */
    uint32_t offset=km_u32(r+8),length=km_u32(r+12);
    if(!length || length>KM_KEY_MAX || !km_range(cap,offset,length,16)) { k->broken=1; goto out; }
    memcpy(blob->data,r+offset,length); blob->length=length; rc=0;
out:
    km_clear(&w,sizeof(w)); km_clear(k->shared,k->capacity); return rc;
}

/* Match the hardware-enforced packed prefix that the installed HAL converts
 * in 0x9398..0x973c. The second (software) set starts at prefix offset 273.
 * Recognize only the observed eight-byte marker and the checked fixed prefix.
 * The TEE authenticates the opaque blob during get-characteristics and use. */
static int km_key_policy(const struct km_blob *blob,uint64_t sid)
{
    const unsigned char *b=blob->data;
    if(blob->length<=394 || blob->length>KM_KEY_MAX || !sid ||
       km_u64(b)!=UINT64_C(0x4b4d4b44)) return -1;
    uint32_t p0=km_u32(b+28),p1=km_u32(b+32);
    if(km_u32(b+24)<9 || km_u32(b+24)>64 ||
       km_u32(b+48)!=2 || !((p0==0 && p1==1) || (p0==1 && p1==0)) ||
       b[52]!=1 || km_u32(b+53)!=32 || b[57]!=1 || km_u32(b+58)!=256 ||
       km_u32(b+82)!=1 || km_u32(b+62)!=32 ||
       km_u32(b+154)!=1 || km_u32(b+122)!=1 ||
       b[158]!=0 || b[159]!=1 || km_u32(b+160)!=128 ||
       km_u32(b+235)!=1 || km_u64(b+195)!=sid ||
       b[239]!=0 || b[240]!=1 || km_u32(b+241)!=1 || b[245]!=0 ||
       b[252]!=1 || km_u32(b+253)!=0) return -1;
    return 0;
}
static int km_recognize(struct km_context *k,const struct km_blob *blob)
{
    struct km_wire w={.used=28};
    const unsigned char *r=NULL; size_t cap=0;
    km_put32(w.data,0x109); km_put32(w.data+4,28);
    km_put32(w.data+8,(uint32_t)blob->length);
    /* Application ID and application data are absent, as at generation. */
    int rc=km_append(&w,blob->data,blob->length);
    if(!rc) rc=km_exchange(k,&w,&r,&cap);
    km_clear(&w,sizeof(w)); km_clear(k->shared,k->capacity); return rc;
}
static int km_begin(struct km_context *k,const struct km_blob *blob,int decrypt,
                    unsigned char nonce[12],uint64_t *operation)
{
    struct km_wire w={.used=24};
    const unsigned char empty_hat[KM_HAT_SIZE]={0};
    struct km_param params[]={
        {KM_BLOCK_MODE,32,NULL,0},{KM_PADDING,1,NULL,0},
        {KM_MAC_LENGTH,128,NULL,0},{KM_NONCE,0,nonce,12},
        {KM_AUTH_TOKEN,0,empty_hat,KM_HAT_SIZE}
    };
    if(!decrypt) params[3]=params[4];
    int rc=-1; const unsigned char *r=NULL; size_t cap=0;
    *operation=0;
    if(!blob->length || blob->length>KM_KEY_MAX) goto out;
    km_put32(w.data,0x10f); km_put32(w.data+4,decrypt ? 1 : 0);
    km_put32(w.data+8,24); km_put32(w.data+12,(uint32_t)blob->length);
    if(km_append(&w,blob->data,blob->length)) goto out;
    km_put32(w.data+16,(uint32_t)w.used); km_put32(w.data+20,decrypt ? 5 : 4);
    if(km_params(&w,params,decrypt ? 5 : 4) || km_exchange(k,&w,&r,&cap)) goto out;
    /* begin_operation 0xaca0..0xacf6: inline nonce, not an offset blob. */
    if(cap<40 || !km_u64(r+8)) { k->broken=1; goto out; }
    *operation=km_u64(r+8);
    uint32_t n=km_u32(r+36);
    if((!decrypt && n!=12) || (decrypt && n!=0 && n!=12) ||
       (decrypt && n==12 && memcmp(nonce,r+20,12))) { k->broken=1; goto out; }
    if(!decrypt) memcpy(nonce,r+20,12);
    rc=0;
out:
    km_clear(&w,sizeof(w)); km_clear(k->shared,k->capacity); return rc;
}
static int km_finish(struct km_context *k,uint64_t operation,
                     const unsigned char hat[KM_HAT_SIZE],
                     const unsigned char *input,size_t input_len,
                     unsigned char *output,size_t expected_len)
{
    struct km_wire w={.used=36};
    const struct km_param param={KM_AUTH_TOKEN,0,hat,KM_HAT_SIZE};
    int rc=-1; const unsigned char *r=NULL; size_t cap=0;
    if(!operation || !input || !output ||
        !((input_len==32 && expected_len==48) || (input_len==48 && expected_len==32))) goto out;
    km_put32(w.data,0x112); km_put64(w.data+4,operation);
    km_put32(w.data+12,36); km_put32(w.data+16,1);
    if(km_params(&w,&param,1)) goto out;
    km_put32(w.data+20,(uint32_t)w.used); km_put32(w.data+24,(uint32_t)input_len);
    /* Signature +28/+32 is empty. GCM authentication tag is part of input. */
    if(km_append(&w,input,input_len) || km_exchange(k,&w,&r,&cap)) goto out;
    uint32_t offset=km_u32(r+8),length=km_u32(r+12);
    if(length!=expected_len || !km_range(cap,offset,length,16)) { k->broken=1; goto out; }
    memcpy(output,r+offset,length); rc=0;
out:
    km_clear(&w,sizeof(w)); km_clear(k->shared,k->capacity); return rc;
}
static int km_abort(struct km_context *k,uint64_t operation)
{
    struct km_wire w={.used=12};
    const unsigned char *r=NULL; size_t cap=0;
    if(!operation || k->broken) return -1;
    km_put32(w.data,0x113); km_put64(w.data+4,operation);
    int rc=km_exchange(k,&w,&r,&cap);
    km_clear(&w,sizeof(w)); km_clear(k->shared,k->capacity); return rc;
}
#endif
