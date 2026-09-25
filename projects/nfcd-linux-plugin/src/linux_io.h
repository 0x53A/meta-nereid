/* SPDX-License-Identifier: BSD-3-Clause */
#ifndef HOKI_LINUX_NFC_IO_H
#define HOKI_LINUX_NFC_IO_H
#include <glib.h>
#include <linux/nfc.h>
#include <netlink/msg.h>

typedef struct {
    guint32 index, protocols;
    gboolean powered;
} LinuxDevice;
typedef struct {
    guint32 index, protocols;
    guint8 sak, uid[10];
    guint uid_len;
} LinuxTag;
typedef struct linux_nfc_control LinuxNfcControl;
LinuxNfcControl* linux_nfc_control_new(void);
void linux_nfc_control_free(LinuxNfcControl* control);

/* These bounded, blocking calls belong on a worker, not the main loop. */
int linux_nfc_command(LinuxNfcControl* control, guint command, guint32 device, guint32 protocols,
    GArray* records);
int linux_nfc_connect(guint32 device, const LinuxTag* tag, guint protocol);
struct nl_sock* linux_nfc_events(int (*callback)(struct nl_msg*, void*),
    void* data);
gboolean linux_nfc_parse_device(struct nl_msg* msg, LinuxDevice* out);
gboolean linux_nfc_parse_tag(struct nl_msg* msg, LinuxTag* out);
gboolean linux_nfc_parse_event(struct nl_msg* msg, guint* command,
    guint32* device, guint32* target);
guint linux_nfc_protocol(const LinuxTag* tag);
/* Kernel rawsock replies contain a leading status byte. */
int linux_nfc_payload(const guint8* packet, gsize len,
    const guint8** payload, gsize* payload_len);
#endif
