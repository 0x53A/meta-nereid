/* Device-bound, explicitly no-user-authentication volume key lifecycle.
 * Author: Lukas Rieger <code@lukasrieger.com>
 * Separate record/policy from PIN storage. No Gatekeeper calls or UID.
 */
#ifndef NEREID_KEYMASTER_DEVICE_VOLUME_H
#define NEREID_KEYMASTER_DEVICE_VOLUME_H
#include "keymaster-volume.h"

static int km_device_volume_run(struct km_context *k,int create,
    const unsigned char *record,size_t record_length,
    km_random_fn random,void *context,struct km_volume *out)
{
    struct km_blob blob={0};
    unsigned char nonce[12]={0},encrypted[48]={0};
    uint64_t operation=0;
    int rc=-1;
    memset(out,0,sizeof(*out));
    if(create) {
        if(record_length || km_generate_policy(k,0,1,&blob) ||
           km_key_policy_kind(&blob,0,1)) goto done;
        if(random(context,out->secret,32)) goto done;
    } else {
        if(!record || record_length<KM_RECORD_HEADER+1 || record_length>KM_RECORD_MAX ||
           memcmp(record,"NDW1",4) || km_u64(record+8)!=0) goto done;
        blob.length=km_u32(record+4);
        if(!blob.length || blob.length>KM_KEY_MAX || record_length!=KM_RECORD_HEADER+blob.length) goto done;
        memcpy(nonce,record+16,12); memcpy(encrypted,record+28,48);
        memcpy(blob.data,record+KM_RECORD_HEADER,blob.length);
        if(km_key_policy_kind(&blob,0,1) || km_recognize(k,&blob)) goto done;
    }
    if(km_begin_policy(k,&blob,!create,1,nonce,&operation)) goto done;
    uint64_t finishing=operation; operation=0;
    if(create) {
        if(km_finish(k,finishing,NULL,out->secret,32,encrypted,48)) goto done;
        memcpy(out->record,"NDW1",4); km_put32(out->record+4,(uint32_t)blob.length);
        memcpy(out->record+16,nonce,12); memcpy(out->record+28,encrypted,48);
        memcpy(out->record+KM_RECORD_HEADER,blob.data,blob.length);
        out->record_length=KM_RECORD_HEADER+blob.length;
    } else if(km_finish(k,finishing,NULL,encrypted,48,out->secret,32)) goto done;
    rc=0;
done:
    if(operation && km_abort(k,operation)) rc=-1;
    if(rc) km_clear(out,sizeof(*out));
    km_clear(&blob,sizeof(blob)); km_clear(nonce,sizeof(nonce));
    km_clear(encrypted,sizeof(encrypted)); km_clear(k->shared,k->capacity);
    return rc;
}
#endif
