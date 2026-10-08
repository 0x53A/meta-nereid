/* Host-only protocol fixtures. No QSEE, keys or filesystem mappings.
 * Author: Lukas Rieger <code@lukasrieger.com>
 */
#include <assert.h>
#include <stdio.h>
#include "keymaster-device-volume.h"
#include "keymaster-config.h"
struct fake { unsigned calls,verifies,abort_calls; int reject,wrong_hat,finish_error,transport_error,weak_key,device; uint32_t fail_command; };
static const uint64_t sid=UINT64_C(0x0102030405060708);
static const uint64_t op=UINT64_C(0x1122334455667788);
static void blob_fixture(unsigned char *b)
{
    memset(b,0,395); km_put32(b,0x4b4d4b44);
    km_put32(b+24,10); km_put32(b+48,2); km_put32(b+32,1);
    b[52]=1; km_put32(b+53,32); b[57]=1; km_put32(b+58,256);
    km_put32(b+82,1); km_put32(b+62,32);
    km_put32(b+154,1); km_put32(b+122,1);
    b[159]=1; km_put32(b+160,128); km_put32(b+235,1); km_put64(b+195,sid);
    b[240]=1; km_put32(b+241,1); b[252]=1;
}
static int fake_send(void *opaque,unsigned char *b,size_t length,size_t capacity)
{
    struct fake *f=opaque; f->calls++;
    unsigned char *r=b+length;
    assert(capacity>=512);
    memset(r,0,capacity);
    if(f->fail_command==km_u32(b)) { km_put32(r,(uint32_t)-21); return 0; }
    switch(km_u32(b)) {
    case 0x116:
        assert(length==48 && km_u32(b+4)==12 && km_u32(b+8)==3);
        assert(km_u32(b+12)==0x300002c1 && km_u32(b+24)==0x300002c2 && km_u32(b+36)==0x300002ce);
        break;
    case 0x108: {
        unsigned count=f->device?8:9;
        assert(length==12+12*count && km_u32(b+4)==12 && km_u32(b+8)==count);
        const uint32_t tags[]={0x20000001,0x20000001,0x10000002,0x30000003,0x20000004,0x20000006,0x30000008,0xa00001f6,0x100001f8};
        const uint64_t values[]={0,1,32,256,32,1,128,sid,1};
        for(unsigned i=0;i<count;i++) {
            assert(km_u32(b+12+12*i)==(f->device && i==7?KM_NO_AUTH:tags[i]));
            assert(km_u64(b+16+12*i)==(f->device && i==7?1:values[i]));
        }
        km_put32(r+8,16); km_put32(r+12,395); blob_fixture(r+16);
        if(f->device) { km_put32(r+16+235,0); r[16+239]=1; r[16+240]=0; }
        if(f->weak_key) r[16+239]=!f->device;
        break;
    }
    case 0x109:
        assert(length==423 && km_u32(b+4)==28 && km_u32(b+8)==395);
        assert(km_u32(b+12)==0 && km_u32(b+16)==0 && km_u32(b+20)==0 && km_u32(b+24)==0);
        break;
    case 0x10f: {
        int decrypt=km_u32(b+4)==1;
        assert(km_u32(b+8)==24 && km_u32(b+12)==395 && km_u32(b+16)==419);
        unsigned count=(decrypt?5:4)-(f->device?1:0);
        assert(km_u32(b+20)==count);
        if(!f->device) {
        const unsigned char *hat=b+419+(count-1)*12;
        assert(km_u32(hat)==0x900003ea && km_u32(hat+8)==69);
        assert(km_u32(hat+4)==(decrypt?72U:48U));
        for(unsigned i=0;i<69;i++) assert(b[419+km_u32(hat+4)+i]==0);
        }
        if(decrypt) {
            const unsigned char *nonce=b+419+36;
            assert(km_u32(nonce)==0x900003e9 && km_u32(nonce+8)==12);
            assert(km_u32(nonce+4)==count*12);
            assert(b[419+km_u32(nonce+4)]==0x19);
        }
        km_put64(r+8,op); memset(r+20,0x19,12); km_put32(r+36,12);
        break;
    }
    case 0x112: {
        assert(km_u64(b+4)==op && km_u32(b+12)==36 && km_u32(b+16)==(f->device?0U:1U));
        if(!f->device) {
        assert(km_u32(b+36)==0x900003ea && km_u32(b+40)==12 && km_u32(b+44)==69);
        assert(km_u64(b+49)==op && km_u64(b+57)==sid);
        }
        unsigned start=f->device?36:117;
        assert(km_u32(b+20)==start && km_u32(b+28)==0 && km_u32(b+32)==0);
        if(f->transport_error) return -1;
        if(f->finish_error) { km_put32(r,(uint32_t)-30); break; }
        size_t out=km_u32(b+24)==32?48:32;
        assert(length==start+km_u32(b+24));
        km_put32(r+8,16); km_put32(r+12,(uint32_t)out); memset(r+16,out==48?0x66:0x42,out);
        break;
    }
    case 0x113:
        f->abort_calls++; assert(length==12 && km_u64(b+4)==op); break;
    default: assert(!"unexpected command");
    }
    return 0;
}
static int fake_verify(void *opaque,uint64_t challenge,unsigned char hat[69],int32_t *status)
{
    struct fake *f=opaque; f->verifies++;
    *status=f->reject==(int)f->verifies ? -30 : 0;
    if(*status) return 0;
    memset(hat,0,69); km_put64(hat+1,challenge+(f->wrong_hat?1:0)); km_put64(hat+9,sid);
    hat[28]=1; memset(hat+37,0x55,32); return 0;
}
static int fake_random(void *opaque,unsigned char *out,size_t length)
{ (void)opaque; memset(out,0x42,length); return 0; }
static int run(struct fake *f,int create,struct km_volume *output)
{
    unsigned char shared[0xa000]={0},record[76+395]={0};
    struct km_context k={.shared=shared,.capacity=sizeof(shared),.send=fake_send,.opaque=f};
    memcpy(record,"NKW1",4); km_put32(record+4,395); km_put64(record+8,sid);
    memset(record+16,0x19,12); memset(record+28,0x66,48); blob_fixture(record+76);
    int rc=km_volume_run(&k,create,create?NULL:record,create?0:sizeof(record),fake_verify,fake_random,f,output);
    if(f->fail_command) { assert(k.failed_command==f->fail_command); assert(k.failed_status==-21); }
    if(f->finish_error) { assert(k.failed_command==0x112); assert(k.failed_status==-30); }
    if(f->transport_error) { assert(k.failed_command==0x112); assert(k.failed_status==INT32_MIN); }
    return rc;
}
static void assert_empty(const struct km_volume *v)
{
    const unsigned char *b=(const unsigned char *)v;
    for(size_t i=0;i<sizeof(*v);i++) assert(b[i]==0);
}
static void device_flows(void)
{
    unsigned char shared[0xa000]={0};
    struct fake f={.device=1};
    struct km_context k={.shared=shared,.capacity=sizeof(shared),.send=fake_send,.opaque=&f};
    struct km_volume wrapped,opened;
    assert(km_device_volume_run(&k,1,NULL,0,fake_random,&f,&wrapped)==0);
    assert(f.calls==3 && f.verifies==0 && wrapped.record_length==471);
    assert(!memcmp(wrapped.record,"NDW1",4) && km_u64(wrapped.record+8)==0);
    assert(km_device_volume_run(&k,0,wrapped.record,wrapped.record_length,fake_random,&f,&opened)==0);
    assert(f.calls==6 && f.verifies==0 && !memcmp(wrapped.secret,opened.secret,32));
    memcpy(wrapped.record,"NKW1",4);
    assert(km_device_volume_run(&k,0,wrapped.record,wrapped.record_length,fake_random,&f,&opened)==-1);
    assert(f.calls==6); assert_empty(&opened);
    memcpy(wrapped.record,"NDW1",4); km_put64(wrapped.record+8,sid);
    assert(km_device_volume_run(&k,0,wrapped.record,wrapped.record_length,fake_random,&f,&opened)==-1);
    assert(f.calls==6); assert_empty(&opened);
    f=(struct fake){.device=1,.weak_key=1};
    assert(km_device_volume_run(&k,1,NULL,0,fake_random,&f,&opened)==-1);
    assert(f.calls==1); assert_empty(&opened);
    struct km_blob blob={.length=395}; blob_fixture(blob.data);
    assert(km_key_policy_kind(&blob,0,1)==-1);
    km_put32(blob.data+235,0); blob.data[239]=1; blob.data[240]=0;
    assert(km_key_policy_kind(&blob,0,1)==0);
    assert(km_key_policy(&blob,sid)==-1);
}
/* Independent vendor ABI fixture: params_serialize 0x8ed0 stores
 * get_indirect_buf_offset_from_main(), whose 0x1994 implementation returns
 * reserved record bytes + indirect bytes used, NOT the command cursor.
 * Provenance: _Tasks/20261003_Keymaster_Request_Encoding/summary.md. */
static void vendor_parameter_offsets(void)
{
    static const unsigned char records[60]={
        0x04,0,0,0x20, 0x20,0,0,0, 0,0,0,0,
        0x06,0,0,0x20, 1,0,0,0, 0,0,0,0,
        0xeb,3,0,0x30, 0x80,0,0,0, 0,0,0,0,
        0xe9,3,0,0x90, 0x3c,0,0,0, 12,0,0,0,
        0xea,3,0,0x90, 0x48,0,0,0, 69,0,0,0
    };
    unsigned char nonce[12],hat[69]={0};
    memset(nonce,0x19,sizeof(nonce));
    const struct km_param params[]={
        {KM_BLOCK_MODE,32,NULL,0},{KM_PADDING,1,NULL,0},
        {KM_MAC_LENGTH,128,NULL,0},{KM_NONCE,0,nonce,12},
        {KM_AUTH_TOKEN,0,hat,69}
    };
    const size_t bases[]={0,31,419};
    for(unsigned i=0;i<3;i++) {
        struct km_wire w={.used=bases[i]};
        memset(w.data,0xa5,bases[i]);
        assert(km_params(&w,params,5)==0);
        assert(w.used==bases[i]+141);
        assert(memcmp(w.data+bases[i],records,sizeof(records))==0);
        assert(memcmp(w.data+bases[i]+60,nonce,12)==0);
        assert(memcmp(w.data+bases[i]+72,hat,69)==0);
        for(size_t j=0;j<bases[i];j++) assert(w.data[j]==0xa5);
    }
}
int main(void)
{
    vendor_parameter_offsets();
    device_flows();
    struct km_volume output;
    struct fake f={0}; assert(run(&f,1,&output)==0);
    assert(f.calls==3 && f.verifies==2 && output.record_length==471 && output.secret[0]==0x42);
    f=(struct fake){0}; assert(run(&f,0,&output)==0);
    assert(f.calls==3 && f.verifies==1 && output.secret[0]==0x42 && output.record_length==0);
    f=(struct fake){.reject=1}; assert(run(&f,1,&output)==1); assert(f.calls==0); assert_empty(&output);
    f=(struct fake){.reject=1}; assert(run(&f,0,&output)==1); assert(f.abort_calls==1 && f.calls==3); assert_empty(&output);
    f=(struct fake){.reject=2}; assert(run(&f,1,&output)==-1); assert(f.abort_calls==1); assert_empty(&output);
    f=(struct fake){.wrong_hat=1}; assert(run(&f,0,&output)==-1); assert(f.abort_calls==1); assert_empty(&output);
    f=(struct fake){.finish_error=1}; assert(run(&f,0,&output)==-1); assert(f.abort_calls==0); assert_empty(&output);
    f=(struct fake){.transport_error=1}; assert(run(&f,0,&output)==-1); assert(f.abort_calls==0); assert_empty(&output);
    f=(struct fake){.weak_key=1}; assert(run(&f,1,&output)==-1); assert(f.calls==1); assert_empty(&output);
    const uint32_t failing_commands[]={0x108,0x10f,0x112};
    for(unsigned i=0;i<3;i++) {
        f=(struct fake){.fail_command=failing_commands[i]};
        assert(run(&f,1,&output)==-1); assert_empty(&output);
        assert(f.calls==i+1 && f.abort_calls==0);
        assert(f.verifies==(i==2?2U:1U));
    }
    struct km_blob blob={.length=395}; blob_fixture(blob.data); assert(km_key_policy(&blob,sid)==0);
    blob.data[245]=1; assert(km_key_policy(&blob,sid)!=0); blob.data[245]=0;
    km_put64(blob.data+195,sid+1); assert(km_key_policy(&blob,sid)!=0);
    unsigned char shared[0xa000]={0};
    struct km_context k={.shared=shared,.capacity=sizeof(shared),.send=fake_send,.opaque=&f};
    const uint32_t versions[]={100000,202101,20210101}; assert(km_configure(&k,versions)==0);
    uint32_t parsed[3];
    static const unsigned char valid[]="100000\n202101\n20210101\n";
    assert(km_parse_config(valid,sizeof(valid)-1,parsed)==0);
    assert(km_parse_config(valid,sizeof(valid)-2,parsed)!=0);
    assert(km_parse_config((const unsigned char *)"0\n0\n0\n",6,parsed)!=0);
    (void)km_read_config; /* Never call the real config reader in host tests. */
    puts("Keymaster PIN/device lifecycle, vendor parameter-offset and policy/config fixtures passed");
    return 0;
}
