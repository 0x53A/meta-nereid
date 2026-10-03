/* Temporary Hoki QSEE RPMB listener. No Android libraries.
 * Author: Lukas Rieger <code@lukasrieger.com>
 * Ordinary TEE-authenticated writes only; no raw command input or provisioning.
 */
#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <errno.h>
#include <fcntl.h>
#include <unistd.h>
#include <signal.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include "uapi/linux/qseecom.h"
#include "uapi/linux/ion.h"
#include "uapi/linux/mmc/ioctl.h"
#include "rpmb-protocol.h"
#include "listener-completion.h"
#include "write-protocol.h"
#include "counter-recovery.h"

_Static_assert(sizeof(void *)==4,"Reviewed ARM32 only");
_Static_assert(sizeof(struct qseecom_register_listener_req)==16,"Listener ABI");
_Static_assert(sizeof(struct ion_allocation_data)==24,"ION ABI");
_Static_assert(sizeof(struct mmc_ioc_rpmb)==216,"RPMB ABI");
_Static_assert(MMC_IOC_RPMB_CMD==0xc0d8b300UL,"Vendor RPMB ioctl");

static volatile sig_atomic_t stop, ticks;
static void interrupted(int signal_number)
{
    if (signal_number==SIGALRM) ++ticks;
    else stop=1;
    alarm(1); /* Bounds the signal-before-RECEIVE race without a second thread. */
}

static int attribute(const char *path,unsigned long *value)
{
    char b[32],*end;
    int fd=open(path,O_RDONLY|O_CLOEXEC);
    if (fd<0) return -1;
    ssize_t n=read(fd,b,sizeof(b)-1);
    close(fd);
    if (n<=0 || n==(ssize_t)sizeof(b)-1) return -1;
    b[n]=0; errno=0; *value=strtoul(b,&end,0);
    if (errno || end==b) return -1;
    while (*end=='\n' || *end==' ' || *end=='\t') ++end;
    return *end ? -1 : 0;
}

static int read_frames(int fd,const struct read_request *r,unsigned char *response)
{
    struct mmc_ioc_rpmb t={0};
    t.cmds[0]=(struct mmc_ioc_cmd){.write_flag=1,.opcode=25,.flags=0x35,.blksz=512,.blocks=1};
    t.cmds[1]=(struct mmc_ioc_cmd){.write_flag=0,.opcode=18,.flags=0x35,.blksz=512,.blocks=r->frames};
    mmc_ioc_cmd_set_data(t.cmds[0],r->frame);
    mmc_ioc_cmd_set_data(t.cmds[1],response);
    /* One attempt only. CMD25 transports a type2/type4 read request. */
    return ioctl(fd,MMC_IOC_RPMB_CMD,&t);
}

static int submit_write(void *context,struct mmc_ioc_rpmb *transaction)
{
    int rc=ioctl(*(int *)context,MMC_IOC_RPMB_CMD,transaction);
    if (rc) perror("RPMB write transport");
    return rc;
}

struct counter_read_context { int fd; unsigned *reads; };

static int submit_counter_read(void *context,const struct read_request *r,unsigned char *response)
{
    struct counter_read_context *c=context;
    ++*c->reads;
    return read_frames(c->fd,r,response);
}

static int write_cancelled(void *context)
{
    (void)context;
    return stop || ticks>=120;
}

static uint32_t frame_counter(const unsigned char *frame)
{
    return (uint32_t)frame[500]<<24 | (uint32_t)frame[501]<<16 |
           (uint32_t)frame[502]<<8 | frame[503];
}

int main(void)
{
    int qfd=-1,ifd=-1,dfd=-1,rfd=-1,registered=0,result=1;
    const size_t allocation_size=0x7000;
    unsigned char *shared=MAP_FAILED;
    unsigned long mult,reliable,enhanced;
    unsigned requests=0,reads=0,writes=0;
    unsigned recovery_pending=0;
    int failed=0;
    setvbuf(stdout,NULL,_IONBF,0);
    if (attribute("/sys/block/mmcblk0/device/raw_rpmb_size_mult",&mult) ||
        attribute("/sys/block/mmcblk0/device/rel_sectors",&reliable) ||
        attribute("/sys/block/mmcblk0/device/enhanced_rpmb_supported",&enhanced) ||
        mult!=0x20 || reliable!=1 || enhanced!=0) {
        fputs("Unexpected device geometry; not registering\n",stderr); goto out;
    }
    rfd=open("/dev/mmcblk0rpmb",O_RDWR|O_CLOEXEC|O_NOFOLLOW);
    if (rfd<0) { perror("open RPMB"); goto out; }
    ifd=open("/dev/ion",O_RDWR|O_CLOEXEC);
    if (ifd<0) { perror("open ION"); goto out; }
    struct ion_allocation_data a={.len=allocation_size,.heap_id_mask=1U<<27,.flags=0};
    if (ioctl(ifd,ION_IOC_ALLOC,&a)) { perror("ION alloc"); goto out; }
    dfd=a.fd;
    shared=mmap(NULL,allocation_size,PROT_READ|PROT_WRITE,MAP_SHARED,dfd,0);
    if (shared==MAP_FAILED) { perror("map ION"); goto out; }
    memset(shared,0,allocation_size);
    qfd=open("/dev/qseecom",O_RDWR|O_CLOEXEC);
    if (qfd<0) { perror("open QSEE"); goto out; }
    struct sigaction sa={.sa_handler=interrupted};
    sigemptyset(&sa.sa_mask);
    if (sigaction(SIGTERM,&sa,NULL) || sigaction(SIGINT,&sa,NULL) || sigaction(SIGALRM,&sa,NULL)) {
        perror("signals"); goto out;
    }
    alarm(1);
    struct qseecom_register_listener_req reg={
        .listener_id=0x2000,.ifd_data_fd=dfd,.virt_sb_base=shared,.sb_size=LISTENER_SIZE
    };
    if (ioctl(qfd,QSEECOM_IOCTL_REGISTER_LISTENER_REQ,&reg)) { perror("register RPMB"); goto out; }
    registered=1;
    printf("READY pid=%ld listener=8192 mode=authenticated-data sectors=8192 reliable=1\n",(long)getpid());
    while (!stop && ticks<120) {
        if (ioctl(qfd,QSEECOM_IOCTL_RECEIVE_REQ,0)) {
            if (errno==EINTR) continue;
            perror("receive"); failed=1; break;
        }
        uint32_t cmd=word(shared);
        ++requests;
        printf("request=%u command=0x%x\n",requests,cmd);
        if (requests>64) failed=1;
        if (recovery_pending) {
            if (requests>64 || stop || ticks>=120) recovery_pending=0;
            struct counter_read_context c={rfd,&reads};
            int rc=recover_counter(shared,&recovery_pending,submit_counter_read,&c);
            printf("write-error counter cleanup transport=%d; stopping\n",rc);
            if (!rc)
                printf("counter response type=0x%04x result=0x%04x counter=%u (TEE validates)\n",
                       frame_type(shared+20),((unsigned)shared[528]<<8)|shared[529],
                       frame_counter(shared+20));
            /* failed stays set: no more reads or writes after this callback. */
        } else if (cmd==0x101) {
            uint32_t version=word(shared+4);
            printf("init version=%u\n",version);
            init_reply(shared,version,failed);
            if (!version) { puts("STOP invalid initialization version"); failed=1; }
        } else if (cmd==0x102) {
            struct read_request r={0};
            uint32_t version=word(shared+16);
            /* Metadata only; never log frames, MACs, nonces or stored payloads. */
            printf("read header frames=%u length=%u offset=%u version=%u\n",
                   word(shared+4),word(shared+8),word(shared+12),version);
            uint32_t offset=word(shared+12);
            if (offset>=16 && offset<=LISTENER_SIZE-512)
                printf("read frame operation_be=0x%04x\n",frame_type(shared+offset));
            else puts("read frame operation unavailable: offset outside bounded frame area");
            if (failed || reads>=32 || decode_read(shared,&r)) {
                const char *reason=read_shape_error(shared);
                printf("STOP read rejected: %s\n",reason ? reason : "run budget/state");
                failed=1; rw_reply(shared,cmd,version,-1,0);
            } else {
                printf("read type=0x%04x frames=%u\n",frame_type(r.frame),r.frames);
                ++reads;
                memset(shared+20,0,r.frames*512);
                int rc=read_frames(rfd,&r,shared+20);
                if (rc) { perror("RPMB read"); failed=1; }
                rw_reply(shared,cmd,version,rc ? -1 : 0,rc ? 0 : r.frames*512);
            }
            explicit_bzero(&r,sizeof(r));
        } else if (cmd==0x103) {
            uint32_t version=word(shared+16);
            struct write_request w={0};
            printf("write header frames=%u length=%u offset=%u version=%u group=%u\n",
                   word(shared+4),word(shared+8),word(shared+12),version,word(shared+20));
            const char *reason=decode_write(shared,&w);
            if (failed || reason || w.frames>256U-writes) {
                printf("STOP write rejected: %s\n",reason ? reason : "run budget/state");
                failed=1; rw_reply(shared,cmd,version,-1,0);
            } else {
                printf("write request counters first=%u last=%u\n",
                       frame_counter(w.data),frame_counter(w.data+(w.frames-1U)*512U));
                enum write_outcome outcome=execute_write(&w,shared+20,submit_write,write_cancelled,&rfd,&writes);
                printf("write outcome=%u transactions_total=%u\n",(unsigned)outcome,writes);
                int has_frame=outcome==WRITE_OK || outcome==WRITE_CARD_ERROR || outcome==WRITE_RESPONSE_TYPE;
                if (has_frame)
                    printf("write response type=0x%04x result=0x%04x counter=%u (TEE validates)\n",
                           frame_type(shared+20),((unsigned)shared[20+508]<<8)|shared[20+509],
                           frame_counter(shared+20));
                /* Preserve every transport-successful result frame for TEE validation, as vendor
                 * does. Stop servicing after any failure; no later batch/retry. */
                rw_reply(shared,cmd,version,has_frame ? 0 : -1,has_frame ? 512 : 0);
                if (outcome!=WRITE_OK) failed=1;
            }
            explicit_bzero(&w,sizeof(w));
            if (failed && requests<64 && !stop && ticks<120) {
                recovery_pending=1;
                puts("STOP_WRITES accepting one TEE counter-read cleanup callback");
            }
        } else if (cmd==0x104) {
            printf("partition query version=%u device=%u: config unavailable\n",word(shared+4),word(shared+8));
            partition_unavailable(shared);
            failed=1; /* Missing configuration is a diagnostic stop condition. */
        } else {
            puts("STOP unknown listener command");
            failed=1; rw_reply(shared,cmd,0,-7,0);
        }
        /* Whole response is already in the registered shared mapping. */
        if (ioctl(qfd,QSEECOM_IOCTL_SEND_RESP_REQ,0)) {
            perror("send response"); failed=1; break;
        }
        if (failed && !recovery_pending) break;
    }
    result=failed ? 1 : 0;
    if (failed && !stop && ticks<120) {
        puts("STOP_SERVICING waiting for runner client-completion signal");
        int wait_result=await_client_completion(&stop,&ticks);
        if (wait_result<0) perror("client-completion wait");
        else if (wait_result>0) puts("STOP client-completion deadline expired");
    }
out:
    alarm(0);
    if (registered && ioctl(qfd,QSEECOM_IOCTL_UNREGISTER_LISTENER_REQ,0)) {
        perror("unregister"); result=1;
    }
    if (qfd>=0) close(qfd); /* Triggers deferred unregistration; kernel owns DMA ref. */
    if (shared!=MAP_FAILED) munmap(shared,allocation_size);
    if (dfd>=0) close(dfd);
    if (ifd>=0) close(ifd);
    if (rfd>=0) close(rfd);
    printf("STOPPED requests=%u reads=%u writes=%u result=%d\n",requests,reads,writes,result);
    return result;
}
