/* One read-only callback for the TEE's failed-write cleanup.
 * Author: Lukas Rieger <code@lukasrieger.com>
 * Include after rpmb-protocol.h. The caller remains failed permanently.
 */
typedef int (*counter_read_submit)(void *, const struct read_request *, unsigned char *);

static int recover_counter(unsigned char *shared, unsigned *pending,
                           counter_read_submit submit, void *context)
{
    struct read_request r={0};
    uint32_t cmd=word(shared), version=word(shared+16);
    unsigned permitted=*pending;
    *pending=0; /* Consumed even if malformed or transport fails: never retry. */
    if (!permitted || cmd!=0x102 || decode_read(shared,&r) ||
        r.frames!=1 || frame_type(r.frame)!=2) {
        explicit_bzero(&r,sizeof(r));
        rw_reply(shared,cmd,version,-1,0);
        return -1;
    }
    memset(shared+20,0,512);
    int rc=submit(context,&r,shared+20);
    /* The TEE validates the full counter response, including authentication. */
    rw_reply(shared,cmd,version,rc ? -1 : 0,rc ? 0 : 512);
    explicit_bzero(&r,sizeof(r));
    return rc;
}
