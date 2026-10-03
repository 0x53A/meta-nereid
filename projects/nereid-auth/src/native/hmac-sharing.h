/* Fixed captured Qualcomm TEE startup ABI; no general command interface.
 * Author: Lukas Rieger <code@lukasrieger.com>
 */
#ifndef HOKI_HMAC_SHARING_H
#define HOKI_HMAC_SHARING_H
#include <stdint.h>
#include <stddef.h>
#include <string.h>
#include <limits.h>

struct sharing_result { unsigned calls; int32_t get_status,compute_status; };
typedef int (*sharing_send)(void *,unsigned char *,size_t,size_t);
static void sharing_u32(unsigned char *p,uint32_t v)
{
    p[0]=(unsigned char)v; p[1]=(unsigned char)(v>>8);
    p[2]=(unsigned char)(v>>16); p[3]=(unsigned char)(v>>24);
}
static int32_t sharing_status(const unsigned char *p)
{
    uint32_t v=(uint32_t)p[0]|((uint32_t)p[1]<<8)|
        ((uint32_t)p[2]<<16)|((uint32_t)p[3]<<24);
    return v<=INT32_MAX ? (int32_t)v : -1-(int32_t)(UINT32_MAX-v);
}
static void sharing_clear(void *p,size_t n)
{
    volatile unsigned char *b=p;
    while (n--) *b++=0;
}
/* Return 0 only when both ordinary setup calls succeed. No retry. Transport
 * receives request length and remaining response capacity, as the vendor does.
 * This API does not report actual bytes written; fixed response ABI is assumed.
 */
static int initialize_sharing(unsigned char *b,size_t size,sharing_send send,
                              void *context,struct sharing_result *out)
{
    unsigned char record[64]={0};
    int rc=-1;
    if (!out) return -1;
    *out=(struct sharing_result){0,INT32_MIN,INT32_MIN};
    if (!b || !send || size<112 || size>UINT32_MAX) goto done;
    memset(b,0,size);
    sharing_u32(b,0x20e);
    sharing_u32(b+4,0x80000000U);
    ++out->calls;
    if (send(context,b,4,size-4)) goto done;
    out->get_status=sharing_status(b+4);
    if (out->get_status) goto done;
    memcpy(record,b+8,sizeof(record));
    memset(b,0,size);
    sharing_u32(b,0x20f); sharing_u32(b+4,12); sharing_u32(b+8,1);
    memcpy(b+12,record,sizeof(record));
    sharing_u32(b+76,0x80000000U);
    ++out->calls;
    if (send(context,b,76,size-76)) goto done;
    out->compute_status=sharing_status(b+76);
    if (out->compute_status) goto done;
    /* The following32 bytes are an agreement check, not the signing key.
     * A singleton has no other participant to compare; never log/retain it. */
    rc=0;
done:
    sharing_clear(record,sizeof(record));
    if (b && size<=UINT32_MAX) sharing_clear(b,size);
    return rc;
}
#endif
