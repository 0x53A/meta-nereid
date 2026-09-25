/* SPDX-License-Identifier: BSD-3-Clause */
#include "linux_target.h"
#include "linux_io.h"
#include <nfc_target_impl.h>
#include <glib-unix.h>
#include <sys/socket.h>
#include <unistd.h>
#include <errno.h>

typedef struct {
    NfcTarget parent;
    int fd;
    guint watch, gone_idle;
    gboolean pending;
} LinuxTarget;
typedef NfcTargetClass LinuxTargetClass;
G_DEFINE_TYPE(LinuxTarget, linux_target, NFC_TYPE_TARGET)

static void close_target(LinuxTarget* self)
{
    if (self->watch) { g_source_remove(self->watch); self->watch = 0; }
    if (self->fd >= 0) { close(self->fd); self->fd = -1; }
    self->pending = FALSE;
}

static gboolean gone_idle(gpointer data)
{
    LinuxTarget* self = data;
    self->gone_idle = 0;
    nfc_target_gone(&self->parent);
    return G_SOURCE_REMOVE;
}

static void retire(LinuxTarget* self)
{
    close_target(self);
    if (!self->gone_idle)
        self->gone_idle = g_idle_add_full(G_PRIORITY_DEFAULT, gone_idle,
            g_object_ref(self), g_object_unref);
}

static gboolean receive_packet(gint fd, GIOCondition condition, gpointer data)
{
    LinuxTarget* self = g_object_ref(data);
    guint8 packet[65537];
    struct iovec iov = { .iov_base = packet, .iov_len = sizeof(packet) };
    struct msghdr msg = { .msg_iov = &iov, .msg_iovlen = 1 };
    const guint8* payload;
    gsize len;
    ssize_t count = recvmsg(fd, &msg, MSG_DONTWAIT);
    if (count < 0 && (errno == EAGAIN || errno == EINTR) &&
        !(condition & (G_IO_ERR | G_IO_HUP | G_IO_NVAL))) {
        g_object_unref(self);
        return G_SOURCE_CONTINUE;
    }
    gboolean pending = self->pending;
    self->pending = FALSE;
    if (count <= 0 || (msg.msg_flags & MSG_TRUNC) || !pending ||
        linux_nfc_payload(packet, count > 0 ? count : 0, &payload, &len)) {
        /* A late packet cannot be mistaken for a later request. */
        retire(self);
        if (pending) nfc_target_transmit_done(&self->parent,
            NFC_TRANSMIT_STATUS_ERROR, NULL, 0);
    } else {
        nfc_target_transmit_done(&self->parent, NFC_TRANSMIT_STATUS_OK,
            payload, len);
    }
    gboolean keep = self->watch != 0;
    g_object_unref(self);
    return keep ? G_SOURCE_CONTINUE : G_SOURCE_REMOVE;
}

static gboolean transmit(NfcTarget* target, const void* data, guint len)
{
    LinuxTarget* self = (LinuxTarget*)target;
    if (self->fd < 0 || !len || len > 65536 || self->pending) return FALSE;
    ssize_t n = send(self->fd, data, len, MSG_DONTWAIT | MSG_NOSIGNAL);
    if (n != (ssize_t)len) { retire(self); return FALSE; }
    self->pending = TRUE;
    return TRUE;
}

static void cancel(NfcTarget* target)
{
    /* Closing invalidates any in-flight response after timeout/cancellation. */
    retire((LinuxTarget*)target);
}

static void deactivate(NfcTarget* target)
{
    close_target((LinuxTarget*)target);
    nfc_target_gone(target);
}

void linux_target_close(NfcTarget* target) { deactivate(target); }

static void dispose(GObject* object)
{
    close_target((LinuxTarget*)object);
    G_OBJECT_CLASS(linux_target_parent_class)->dispose(object);
}

static void linux_target_init(LinuxTarget* self) { self->fd = -1; }
static void linux_target_class_init(LinuxTargetClass* klass)
{
    G_OBJECT_CLASS(klass)->dispose = dispose;
    klass->transmit = transmit;
    klass->cancel_transmit = cancel;
    klass->deactivate = deactivate;
}

NfcTarget* linux_target_new(int fd, guint protocol, GObject* owner)
{
    LinuxTarget* self = g_object_new(linux_target_get_type(), NULL);
    if (owner) g_object_set_data_full(G_OBJECT(self), "linux-nfc-owner",
        g_object_ref(owner), g_object_unref);
    self->fd = fd;
    self->parent.technology = protocol == NFC_PROTO_ISO14443_B ?
        NFC_TECHNOLOGY_B : NFC_TECHNOLOGY_A;
    self->parent.protocol = protocol == NFC_PROTO_ISO14443_B ?
        NFC_PROTOCOL_T4B_TAG : protocol == NFC_PROTO_ISO14443 ?
        NFC_PROTOCOL_T4A_TAG : NFC_PROTOCOL_T2_TAG;
    self->watch = g_unix_fd_add(fd, G_IO_IN | G_IO_ERR | G_IO_HUP | G_IO_NVAL,
        receive_packet, self);
    nfc_target_set_transmit_timeout(&self->parent, 2000);
    return &self->parent;
}
