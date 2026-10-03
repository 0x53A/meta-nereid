/* Ordinary authenticated RPMB writes supplied by the resident TEE only.
 * Author: Lukas Rieger <code@lukasrieger.com>
 * Include after rpmb-protocol.h and the downstream MMC ioctl UAPI.
 */
#define MAX_WRITE_FRAMES ((LISTENER_SIZE-24U)/512U)
struct write_request {
    uint32_t frames, version;
    unsigned char data[MAX_WRITE_FRAMES*512U];
};

static const char *decode_write(const unsigned char *b, struct write_request *r)
{
    uint32_t count=word(b+4), offset=word(b+12), group=word(b+20);
    if (word(b)!=0x103) return "command";
    if (!count || count>MAX_WRITE_FRAMES) return "frame count";
    if (offset<24 || offset>LISTENER_SIZE || count>(LISTENER_SIZE-offset)/512U)
        return "request offset/count";
    /* Conservative policy for the recorded eMMC configuration. Do not split
     * a larger authenticated group or infer its size from header word+8. */
    if (group!=1) return "unreviewed authenticated group size";
    for (uint32_t i=0;i<count;++i)
        if (frame_type(b+offset+i*512U)!=3) return "not authenticated data write";
    r->frames=count; r->version=word(b+16);
    /* Validate all frames first, then copy before the shared response aliases
     * this request. Counters, MACs and payload bytes remain untouched. */
    memcpy(r->data,b+offset,count*512U);
    return NULL;
}

enum write_outcome { WRITE_OK, WRITE_TRANSPORT_ERROR, WRITE_CARD_ERROR, WRITE_RESPONSE_TYPE, WRITE_CANCELLED };
typedef int (*rpmb_submit)(void *context,struct mmc_ioc_rpmb *transaction);
typedef int (*rpmb_cancelled)(void *context);

static enum write_outcome execute_write(const struct write_request *r,
                                        unsigned char response[512],
                                        rpmb_submit submit,rpmb_cancelled cancelled,void *context,
                                        unsigned *attempted)
{
    unsigned char result_request[512]={0};
    result_request[511]=5; /* Fixed result-read request recovered from vendor. */
    for (uint32_t i=0;i<r->frames;++i) {
        if (cancelled(context)) return WRITE_CANCELLED;
        struct mmc_ioc_rpmb t={0};
        t.cmds[0]=(struct mmc_ioc_cmd){.write_flag=(int)0x80000001U,
            .opcode=25,.flags=0x35,.blksz=512,.blocks=1};
        t.cmds[1]=(struct mmc_ioc_cmd){.write_flag=1,
            .opcode=25,.flags=0x35,.blksz=512,.blocks=1};
        t.cmds[2]=(struct mmc_ioc_cmd){.write_flag=0,
            .opcode=18,.flags=0x35,.blksz=512,.blocks=1};
        mmc_ioc_cmd_set_data(t.cmds[0],r->data+i*512U);
        mmc_ioc_cmd_set_data(t.cmds[1],result_request);
        mmc_ioc_cmd_set_data(t.cmds[2],response);
        memset(response,0,512);
        ++*attempted;
        if (submit(context,&t)) return WRITE_TRANSPORT_ERROR; /* Never retry. */
        if (frame_type(response)!=0x0300) return WRITE_RESPONSE_TYPE;
        if (response[508] || response[509]) return WRITE_CARD_ERROR;
    }
    return WRITE_OK;
}
