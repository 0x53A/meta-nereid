/* Authentication-bound 32-byte volume secret lifecycle. No hardware-specific
 * calls here: production supplies reviewed transport and Gatekeeper callbacks.
 * Author: Lukas Rieger <code@lukasrieger.com>
 */
#ifndef NEREID_KEYMASTER_VOLUME_H
#define NEREID_KEYMASTER_VOLUME_H
#include "keymaster-wire.h"

typedef int (*km_verify_fn)(void *,uint64_t,unsigned char[69],int32_t *);
typedef int (*km_random_fn)(void *,unsigned char *,size_t);
struct km_volume {
    unsigned char secret[32];
    unsigned char record[KM_RECORD_MAX];
    size_t record_length;
};
static int km_hat_valid(const unsigned char hat[69],uint64_t challenge,uint64_t sid)
{
    return hat[0]==0 && km_u64(hat+1)==challenge && km_u64(hat+9)!=0 &&
        (!sid || km_u64(hat+9)==sid) &&
        hat[25]==0 && hat[26]==0 && hat[27]==0 && hat[28]==1;
}
/* Return 1 only for an ordinary rejected Gatekeeper verification whose cleanup
 * succeeded; -1 means stop/recovery. No automatic retry or fallback key exists. */
static int km_volume_run(struct km_context *k,int create,
    const unsigned char *record,size_t record_length,
    km_verify_fn verify,km_random_fn random,void *context,struct km_volume *out)
{
    struct km_blob blob={0};
    unsigned char hat[69]={0},nonce[12]={0},encrypted[48]={0};
    unsigned char challenge_bytes[8]={0};
    uint64_t sid=0,operation=0;
    int32_t status=INT32_MIN;
    int rc=-1;
    memset(out,0,sizeof(*out));
    if(create) {
        if(record_length || random(context,challenge_bytes,8)) goto done;
        uint64_t challenge=km_u64(challenge_bytes);
        if(verify(context,challenge,hat,&status)) { k->broken=1; goto done; }
        if(status==-30) { rc=1; goto done; }
        if(status || !km_hat_valid(hat,challenge,0)) goto done;
        sid=km_u64(hat+9);
        km_clear(hat,sizeof(hat));
        if(km_generate(k,sid,&blob) || km_key_policy(&blob,sid)) goto done;
        if(random(context,out->secret,sizeof(out->secret))) goto done;
    } else {
        if(!record || record_length<KM_RECORD_HEADER+1 || record_length>KM_RECORD_MAX ||
            memcmp(record,"NKW1",4)) goto done;
        blob.length=km_u32(record+4); sid=km_u64(record+8);
        if(!sid || !blob.length || blob.length>KM_KEY_MAX || record_length!=KM_RECORD_HEADER+blob.length) goto done;
        memcpy(nonce,record+16,12); memcpy(encrypted,record+28,48);
        memcpy(blob.data,record+KM_RECORD_HEADER,blob.length);
        if(km_key_policy(&blob,sid) || km_recognize(k,&blob)) goto done;
    }
    if(km_begin(k,&blob,!create,nonce,&operation)) goto done;
    status=INT32_MIN;
    if(verify(context,operation,hat,&status)) { k->broken=1; goto done; }
    if(status || !km_hat_valid(hat,operation,sid)) {
        if(status==-30 && !create) rc=1;
        goto done;
    }
    /* finish consumes the operation even on a returned Keymaster error. Never
     * release provisional decrypt bytes: only its successful final response. */
    uint64_t finishing=operation; operation=0;
    if(create) {
        if(km_finish(k,finishing,hat,out->secret,32,encrypted,48)) goto done;
        memcpy(out->record,"NKW1",4); km_put32(out->record+4,(uint32_t)blob.length);
        km_put64(out->record+8,sid); memcpy(out->record+16,nonce,12);
        memcpy(out->record+28,encrypted,48);
        memcpy(out->record+KM_RECORD_HEADER,blob.data,blob.length);
        out->record_length=KM_RECORD_HEADER+blob.length;
    } else if(km_finish(k,finishing,hat,encrypted,48,out->secret,32)) goto done;
    rc=0;
done:
    if(operation && km_abort(k,operation)) rc=-1;
    if(rc) km_clear(out,sizeof(*out));
    km_clear(&blob,sizeof(blob)); km_clear(hat,sizeof(hat));
    km_clear(nonce,sizeof(nonce)); km_clear(encrypted,sizeof(encrypted));
    km_clear(challenge_bytes,sizeof(challenge_bytes));
    km_clear(k->shared,k->capacity);
    return rc;
}
#endif
