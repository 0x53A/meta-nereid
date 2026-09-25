/* SPDX-License-Identifier: BSD-3-Clause */
#include "linux_io.h"
#include <errno.h>
#include <poll.h>
#include <sys/socket.h>
#include <unistd.h>
#include <netlink/genl/genl.h>
#include <netlink/genl/ctrl.h>

struct linux_nfc_control {
    struct nl_sock* socket;
    int family;
};

LinuxNfcControl* linux_nfc_control_new(void) { return g_new0(LinuxNfcControl, 1); }
void linux_nfc_control_free(LinuxNfcControl* control)
{
    if (control->socket) nl_socket_free(control->socket);
    g_free(control);
}

static struct nla_policy policy[NFC_ATTR_MAX + 1] = {
    [NFC_ATTR_DEVICE_INDEX] = { .type = NLA_U32 },
    [NFC_ATTR_PROTOCOLS] = { .type = NLA_U32 },
    [NFC_ATTR_DEVICE_POWERED] = { .type = NLA_U8 },
    [NFC_ATTR_TARGET_INDEX] = { .type = NLA_U32 },
    [NFC_ATTR_TARGET_SEL_RES] = { .type = NLA_U8 },
    [NFC_ATTR_TARGET_NFCID1] = { .type = NLA_BINARY, .maxlen = 10 }
};

static gboolean parse(struct nl_msg* msg, struct nlattr** attrs)
{
    return genlmsg_parse(nlmsg_hdr(msg), 0, attrs, NFC_ATTR_MAX, policy) >= 0;
}

gboolean linux_nfc_parse_device(struct nl_msg* msg, LinuxDevice* out)
{
    struct nlattr* attrs[NFC_ATTR_MAX + 1];
    if (!parse(msg, attrs) || !attrs[NFC_ATTR_DEVICE_INDEX] ||
        !attrs[NFC_ATTR_PROTOCOLS] || !attrs[NFC_ATTR_DEVICE_POWERED])
        return FALSE;
    *out = (LinuxDevice) { .index = nla_get_u32(attrs[NFC_ATTR_DEVICE_INDEX]),
        .protocols = nla_get_u32(attrs[NFC_ATTR_PROTOCOLS]),
        .powered = !!nla_get_u8(attrs[NFC_ATTR_DEVICE_POWERED]) };
    return TRUE;
}

gboolean linux_nfc_parse_tag(struct nl_msg* msg, LinuxTag* out)
{
    struct nlattr* attrs[NFC_ATTR_MAX + 1];
    if (!parse(msg, attrs) || !attrs[NFC_ATTR_TARGET_INDEX] ||
        !attrs[NFC_ATTR_PROTOCOLS]) return FALSE;
    *out = (LinuxTag) { .index = nla_get_u32(attrs[NFC_ATTR_TARGET_INDEX]),
        .protocols = nla_get_u32(attrs[NFC_ATTR_PROTOCOLS]) };
    if (attrs[NFC_ATTR_TARGET_SEL_RES])
        out->sak = nla_get_u8(attrs[NFC_ATTR_TARGET_SEL_RES]);
    if (attrs[NFC_ATTR_TARGET_NFCID1]) {
        out->uid_len = nla_len(attrs[NFC_ATTR_TARGET_NFCID1]);
        memcpy(out->uid, nla_data(attrs[NFC_ATTR_TARGET_NFCID1]), out->uid_len);
    }
    return TRUE;
}

gboolean linux_nfc_parse_event(struct nl_msg* msg, guint* command,
    guint32* device, guint32* target)
{
    struct nlattr* attrs[NFC_ATTR_MAX + 1];
    if (!parse(msg, attrs) || !attrs[NFC_ATTR_DEVICE_INDEX]) return FALSE;
    *command = ((struct genlmsghdr*) nlmsg_data(nlmsg_hdr(msg)))->cmd;
    *device = nla_get_u32(attrs[NFC_ATTR_DEVICE_INDEX]);
    *target = attrs[NFC_ATTR_TARGET_INDEX] ?
        nla_get_u32(attrs[NFC_ATTR_TARGET_INDEX]) : G_MAXUINT32;
    return TRUE;
}

guint linux_nfc_protocol(const LinuxTag* tag)
{
    if (tag->protocols & NFC_PROTO_ISO14443_MASK) return NFC_PROTO_ISO14443;
    if (tag->protocols & NFC_PROTO_ISO14443_B_MASK) return NFC_PROTO_ISO14443_B;
    /* The Linux MIFARE bit includes Classic, which isn't a Type 2 tag. */
    if ((tag->protocols & NFC_PROTO_MIFARE_MASK) && !tag->sak &&
        (tag->uid_len == 4 || tag->uid_len == 7 || tag->uid_len == 10))
        return NFC_PROTO_MIFARE;
    return 0;
}

int linux_nfc_payload(const guint8* packet, gsize len,
    const guint8** payload, gsize* payload_len)
{
    *payload = NULL;
    *payload_len = 0;
    if (!len) return -EPROTO;
    if (packet[0]) return -EIO;
    *payload = packet + 1;
    *payload_len = len - 1;
    return 0;
}

static struct nl_sock* open_socket(void)
{
    struct nl_sock* sk = nl_socket_alloc();
    struct timeval timeout = { .tv_sec = 2 };
    if (!sk) return NULL;
    if (genl_connect(sk) < 0 || setsockopt(nl_socket_get_fd(sk), SOL_SOCKET,
        SO_RCVTIMEO, &timeout, sizeof(timeout)) < 0) {
        nl_socket_free(sk);
        return NULL;
    }
    return sk;
}

typedef struct {
    guint command;
    gboolean done, dump;
    int status;
    GArray* records;
} Transaction;

static int ack(struct nl_msg* msg, void* data)
{
    Transaction* tx = data;
    (void)msg;
    if (!tx->dump) tx->done = TRUE;
    return NL_OK;
}

static int finish(struct nl_msg* msg, void* data)
{
    Transaction* tx = data;
    struct nlmsghdr* hdr = nlmsg_hdr(msg);
    tx->done = TRUE;
    if (hdr->nlmsg_flags & NLM_F_DUMP_INTR) tx->status = -EINTR;
    if (nlmsg_datalen(hdr) >= (int)sizeof(int)) {
        int status;
        memcpy(&status, nlmsg_data(hdr), sizeof(status));
        if (status) tx->status = status;
    }
    return NL_OK;
}

static int error(struct sockaddr_nl* addr, struct nlmsgerr* err, void* data)
{
    Transaction* tx = data;
    (void)addr;
    tx->status = err->error;
    tx->done = TRUE;
    return NL_STOP;
}

static int record(struct nl_msg* msg, void* data)
{
    Transaction* tx = data;
    LinuxDevice device;
    LinuxTag tag;
    if (tx->records) {
        if (tx->command == NFC_CMD_GET_DEVICE && linux_nfc_parse_device(msg, &device))
            g_array_append_val(tx->records, device);
        else if (tx->command == NFC_CMD_GET_TARGET && linux_nfc_parse_tag(msg, &tag))
            g_array_append_val(tx->records, tag);
        else {
            tx->status = -EPROTO;
            tx->done = TRUE;
        }
    }
    return NL_OK;
}

int linux_nfc_command(LinuxNfcControl* control, guint command, guint32 device, guint32 protocols,
    GArray* records)
{
    /* Keep this socket alive: Linux binds polling ownership to its port ID. */
    if (!control->socket) {
        control->socket = open_socket();
        if (control->socket)
            control->family = genl_ctrl_resolve(control->socket, NFC_GENL_NAME);
    }
    struct nl_sock* sk = control->socket;
    struct nl_msg* msg = NULL;
    int family, rc = -EIO;
    Transaction tx = { .command = command, .dump = records != NULL,
        .records = records };
    if (!sk) return rc;
    family = control->family;
    if (family < 0) { rc = -ENODEV; goto out; }
    msg = nlmsg_alloc();
    if (!msg) { rc = -ENOMEM; goto out; }
    if (!genlmsg_put(msg, NL_AUTO_PORT, NL_AUTO_SEQ, family, 0,
        NLM_F_ACK | (tx.dump ? NLM_F_DUMP : 0), command, NFC_GENL_VERSION) ||
        (command != NFC_CMD_GET_DEVICE &&
         nla_put_u32(msg, NFC_ATTR_DEVICE_INDEX, device) < 0) ||
        (command == NFC_CMD_START_POLL &&
         nla_put_u32(msg, NFC_ATTR_IM_PROTOCOLS, protocols) < 0)) {
        rc = -ENOMEM;
        goto out;
    }
    nl_socket_modify_cb(sk, NL_CB_VALID, NL_CB_CUSTOM, record, &tx);
    nl_socket_modify_cb(sk, NL_CB_ACK, NL_CB_CUSTOM, ack, &tx);
    nl_socket_modify_cb(sk, NL_CB_FINISH, NL_CB_CUSTOM, finish, &tx);
    nl_socket_modify_err_cb(sk, NL_CB_CUSTOM, error, &tx);
    if (nl_send_auto(sk, msg) < 0 || nl_socket_set_nonblocking(sk) < 0) goto out;
    gint64 deadline = g_get_monotonic_time() + 2 * G_TIME_SPAN_SECOND;
    while (!tx.done) {
        struct pollfd pfd = { .fd = nl_socket_get_fd(sk), .events = POLLIN };
        gint64 remaining = deadline - g_get_monotonic_time();
        int ready;
        if (remaining <= 0) { rc = -ETIMEDOUT; goto out; }
        ready = poll(&pfd, 1, (remaining + 999) / 1000);
        if (ready < 0 && errno == EINTR) continue;
        if (ready <= 0) { rc = ready ? -errno : -ETIMEDOUT; goto out; }
        if (pfd.revents & (POLLERR | POLLHUP | POLLNVAL)) goto out;
        int ret = nl_recvmsgs_default(sk);
        if (ret < 0 && ret != -NLE_AGAIN && !tx.done) goto out;
    }
    rc = tx.status;
out:
    if (msg) nlmsg_free(msg);
    return rc;
}

struct nl_sock* linux_nfc_events(int (*callback)(struct nl_msg*, void*), void* data)
{
    struct nl_sock* sk = open_socket();
    int group;
    if (!sk) return NULL;
    group = genl_ctrl_resolve_grp(sk, NFC_GENL_NAME, NFC_GENL_MCAST_EVENT_NAME);
    if (group < 0 || nl_socket_add_membership(sk, group) < 0 ||
        nl_socket_set_nonblocking(sk) < 0) {
        nl_socket_free(sk);
        return NULL;
    }
    nl_socket_disable_seq_check(sk);
    nl_socket_modify_cb(sk, NL_CB_VALID, NL_CB_CUSTOM, callback, data);
    return sk;
}

int linux_nfc_connect(guint32 device, const LinuxTag* tag, guint protocol)
{
    struct sockaddr_nfc addr = { .sa_family = AF_NFC, .dev_idx = device,
        .target_idx = tag->index, .nfc_protocol = protocol };
    int fd = socket(AF_NFC, SOCK_SEQPACKET | SOCK_NONBLOCK | SOCK_CLOEXEC,
        NFC_SOCKPROTO_RAW);
    if (fd < 0) return -errno;
    if (connect(fd, (struct sockaddr*)&addr, sizeof(addr)) < 0) {
        int status = -errno;
        close(fd);
        return status;
    }
    return fd;
}
