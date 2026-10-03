/* Target-specific diagnostic protocol, based on installed Hoki librpmb.so.
 * Author: Lukas Rieger <code@lukasrieger.com>
 */
#include <stdint.h>
#include <string.h>

#define LISTENER_SIZE 0x6400U
/* Capacity of the response area, not an arbitrary diagnostic frame cap. */
#define MAX_READ_FRAMES ((LISTENER_SIZE-20U)/512U)
static uint32_t word(const void *p) { uint32_t v; memcpy(&v,p,4); return v; }
static void setword(void *p,uint32_t v) { memcpy(p,&v,4); }
static unsigned frame_type(const unsigned char *p) { return ((unsigned)p[510]<<8)|p[511]; }

static void init_reply(unsigned char *b,uint32_t requested_version,int failed)
{
    /* Vendor dispatch: v1=20 bytes; nonzero other versions negotiate v2=40.
     * V2 defines first six words. Its final 16 bytes have no initialized fields
     * in the installed binary; our reply zeroes them, never sends stack residue.
     */
    memset(b,0,40);
    setword(b,0x101); setword(b+4,requested_version>1 ? 2 : 1);
    setword(b+8,failed || !requested_version ? UINT32_MAX : 0);
    if (!failed && requested_version) {
        setword(b+12,8192); setword(b+16,1);
        if (requested_version>1) setword(b+20,3); /* eMMC RPMB device type */
    }
}

struct read_request {
    uint32_t frames, version;
    unsigned char frame[512];
};

/* Copies the single outgoing read request away from overlapping response space.
 * Rejects every operation except standard counter-read and authenticated-read.
 */
static const char *read_shape_error(const unsigned char *b)
{
    uint32_t frames=word(b+4), offset=word(b+12);
    if (word(b)!=0x102) return "command";
    if (frames==0 || frames>MAX_READ_FRAMES)
        return "frame count";
    /* Installed vendor READ ignores word+8 as input. Actual TEE request used
     * 17408 for 34 response frames. Independently bound the fixed outgoing
     * 512-byte frame here and the response count above; never size I/O from
     * the ignored declaration. */
    if (offset<24 || offset>LISTENER_SIZE-512) return "request offset";
    unsigned type=frame_type(b+offset);
    if (type!=2 && type!=4) return "frame operation";
    if (type==2 && frames!=1) return "counter response count";
    return NULL;
}

static int decode_read(const unsigned char *b, struct read_request *r)
{
    if (read_shape_error(b)) return -1;
    r->frames=word(b+4); r->version=word(b+16);
    memcpy(r->frame,b+word(b+12),512);
    return 0;
}

static void rw_reply(unsigned char *b,uint32_t cmd,uint32_t version,int status,uint32_t length)
{
    setword(b,cmd); setword(b+4,(uint32_t)status); setword(b+8,length);
    setword(b+12,20); setword(b+16,version);
}

static void partition_unavailable(unsigned char *b)
{
    /* Mirrors the vendor absent-config result; never invent a partition map. */
    setword(b,0x104); setword(b+4,UINT32_MAX); setword(b+8,0); setword(b+12,16);
}
