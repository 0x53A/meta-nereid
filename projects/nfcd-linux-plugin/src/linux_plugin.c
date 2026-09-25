/* SPDX-License-Identifier: BSD-3-Clause */
#include "linux_io.h"
#include "linux_target.h"
#include <nfc_adapter_impl.h>
#include <nfc_plugin_impl.h>
#include <nfc_manager.h>
#include <nfc_target_impl.h>
#include <nfc_tag.h>
#include <nfc_tag_t4.h>
#include <gio/gio.h>
#include <glib-unix.h>
#include <netlink/socket.h>
#include <unistd.h>
#include <errno.h>

#define READER_PROTOCOLS (NFC_PROTO_MIFARE_MASK | NFC_PROTO_ISO14443_MASK | \
    NFC_PROTO_ISO14443_B_MASK)
#define CONNECT_TARGET 0x100

typedef struct {
    NfcAdapter parent;
    guint32 index, protocols;
    guint allowed, generation;
    gboolean want_power, want_reader, polling, busy, stopped;
    gboolean power_pending, mode_pending, discover;
    gboolean failed;
    NfcTarget* target;
    guint32 target_index;
    gulong removed_handler;
    LinuxNfcControl* control;
    NfcPlugin* plugin; /* Keep module loaded while asynchronous work remains. */
    gboolean clearing, repoll;
} LinuxAdapter;
typedef NfcAdapterClass LinuxAdapterClass;
G_DEFINE_TYPE(LinuxAdapter, linux_adapter, NFC_TYPE_ADAPTER)

typedef struct {
    guint command, generation, protocol;
    guint32 device, protocols;
    LinuxTag tag;
    GArray* records;
    int status, fd;
    LinuxNfcControl* control;
} Job;

static void reconcile(LinuxAdapter* self);
static void submit(LinuxAdapter* self, guint command, const LinuxTag* tag);

static void job_free(gpointer data)
{
    Job* job = data;
    if (job->fd >= 0) close(job->fd);
    if (job->records) g_array_unref(job->records);
    g_free(job);
}

static void worker(GTask* task, gpointer source, gpointer data, GCancellable* cancel)
{
    Job* job = data;
    (void)source; (void)cancel;
    if (job->command == CONNECT_TARGET) {
        int fd = linux_nfc_connect(job->device, &job->tag, job->protocol);
        if (fd >= 0) job->fd = fd;
        else job->status = fd;
    } else {
        LinuxNfcControl* control = job->control ? job->control : linux_nfc_control_new();
        job->status = linux_nfc_command(control, job->command, job->device,
            job->protocols, job->records);
        if (!job->control) linux_nfc_control_free(control);
    }
    g_task_return_boolean(task, TRUE);
}

static void clear_target(LinuxAdapter* self)
{
    if (self->target) {
        NfcTarget* target = self->target;
        self->target = NULL;
        self->clearing = TRUE;
        linux_target_close(target);
        nfc_target_unref(target);
        self->clearing = FALSE;
    }
}

static void removed(NfcAdapter* adapter, NfcTag* tag, void* data)
{
    LinuxAdapter* self = (LinuxAdapter*)adapter;
    (void)data;
    if (self->target == tag->target) {
        nfc_target_unref(self->target);
        self->target = NULL;
        if (!self->clearing) reconcile(self);
    }
}

static guint protocols(LinuxAdapter* self)
{
    return self->protocols &
        ((self->allowed & NFC_TECHNOLOGY_A ?
          NFC_PROTO_MIFARE_MASK | NFC_PROTO_ISO14443_MASK : 0) |
         (self->allowed & NFC_TECHNOLOGY_B ? NFC_PROTO_ISO14443_B_MASK : 0));
}

static void publish_target(LinuxAdapter* self, Job* job)
{
    NfcTag* tag = NULL;
    NfcParamPollA a = { .sel_res = job->tag.sak,
        .nfcid1 = { job->tag.uid, job->tag.uid_len } };
    self->target = linux_target_new(job->fd, job->protocol, G_OBJECT(self->plugin));
    self->target_index = job->tag.index;
    job->fd = -1;
    if (job->protocol == NFC_PROTO_MIFARE)
        tag = nfc_adapter_add_tag_t2(&self->parent, self->target, &a);
    else if (job->protocol == NFC_PROTO_ISO14443) {
        /* Linux doesn't expose ATS. Kernel/controller handles fragmentation. */
        NfcParamIsoDepPollA iso = { .fsc = 256 };
        tag = nfc_adapter_add_tag_t4a(&self->parent, self->target, &a, &iso);
    } else {
        NfcParamPollB b = { .fsc = 256 };
        tag = nfc_adapter_add_tag_t4b(&self->parent, self->target, &b, NULL);
    }
    if (!tag) clear_target(self);
}

static void complete(GObject* object, GAsyncResult* result, gpointer data)
{
    LinuxAdapter* self = (LinuxAdapter*)object;
    Job* job = g_task_get_task_data(G_TASK(result));
    (void)data;
    self->busy = FALSE;
    int rc = job->status;
    /* DEV_UP/DOWN report EALREADY when already in that state. */
    if ((job->command == NFC_CMD_DEV_UP || job->command == NFC_CMD_DEV_DOWN) &&
        rc == -EALREADY) rc = 0;
    if (job->command == NFC_CMD_STOP_POLL && rc == -EINVAL) rc = 0;
    if (rc) {
        g_warning("linux-nfc: nfc%u operation %u: %s", self->index,
            job->command, g_strerror(-rc));
        self->failed = TRUE; /* Don't spin on a wedged controller. */
    } else switch (job->command) {
    case NFC_CMD_DEV_UP:
    case NFC_CMD_DEV_DOWN: {
        gboolean powered = job->command == NFC_CMD_DEV_UP;
        gboolean requested = self->power_pending && powered == self->want_power;
        if (requested) self->power_pending = FALSE;
        if (!powered) self->polling = FALSE;
        nfc_adapter_power_notify(&self->parent, powered, requested);
        break;
    }
    case NFC_CMD_START_POLL: self->polling = !self->discover; break;
    case NFC_CMD_STOP_POLL: self->polling = FALSE; self->repoll = FALSE; break;
    case NFC_CMD_GET_TARGET:
        if (!self->stopped && job->generation == self->generation &&
            self->want_power && self->want_reader && !self->target) {
            for (guint i = 0; i < job->records->len; i++) {
                LinuxTag* tag = &g_array_index(job->records, LinuxTag, i);
                if (linux_nfc_protocol(tag) &&
                    ((1u << linux_nfc_protocol(tag)) & protocols(self))) {
                    submit(self, CONNECT_TARGET, tag);
                    return;
                }
            }
            /* Unsupported targets need an explicit restart, no retry storm. */
            self->failed = TRUE;
        }
        break;
    case CONNECT_TARGET:
        if (!self->stopped && job->generation == self->generation &&
            self->want_power && self->want_reader)
            publish_target(self, job);
        else if (job->fd >= 0) {
            close(job->fd);
            job->fd = -1;
        }
        break;
    }
    if (self->failed) {
        if (self->power_pending) {
            self->power_pending = FALSE;
            nfc_adapter_power_notify(&self->parent, self->parent.powered, TRUE);
        }
        if (self->mode_pending) {
            self->mode_pending = FALSE;
            nfc_adapter_mode_notify(&self->parent,
                self->polling || self->target ? NFC_MODE_READER_WRITER : NFC_MODE_NONE,
                TRUE);
        }
    }
    reconcile(self);
}

static void submit(LinuxAdapter* self, guint command, const LinuxTag* tag)
{
    Job* job = g_new0(Job, 1);
    GTask* task = g_task_new(self, NULL, complete, NULL);
    job->command = command;
    job->device = self->index;
    job->protocols = protocols(self);
    job->generation = self->generation;
    job->fd = -1;
    job->control = self->control;
    if (command == NFC_CMD_GET_TARGET)
        job->records = g_array_new(FALSE, FALSE, sizeof(LinuxTag));
    if (tag) { job->tag = *tag; job->protocol = linux_nfc_protocol(tag); }
    self->busy = TRUE;
    g_task_set_task_data(task, job, job_free);
    g_task_run_in_thread(task, worker);
    g_object_unref(task);
}

static void reconcile(LinuxAdapter* self)
{
    if (self->busy || self->failed || self->clearing) return;
    if ((!self->want_power || !self->want_reader) && self->target)
        clear_target(self);
    if ((!self->want_power || !self->want_reader || !protocols(self) || self->repoll) && self->polling) {
        submit(self, NFC_CMD_STOP_POLL, NULL);
        return;
    }
    if (self->parent.powered != self->want_power) {
        submit(self, self->want_power ? NFC_CMD_DEV_UP : NFC_CMD_DEV_DOWN, NULL);
        return;
    }
    if (self->discover) {
        self->discover = FALSE;
        if (self->want_power && self->want_reader && !self->target) {
            submit(self, NFC_CMD_GET_TARGET, NULL);
            return;
        }
    }
    if (self->want_power && self->want_reader && protocols(self) &&
        !self->polling && !self->target) {
        self->repoll = FALSE;
        submit(self, NFC_CMD_START_POLL, NULL);
        return;
    }
    if (self->power_pending) {
        self->power_pending = FALSE;
        nfc_adapter_power_notify(&self->parent, self->parent.powered, TRUE);
    }
    NFC_MODE mode = self->want_power && self->want_reader && protocols(self) ?
        NFC_MODE_READER_WRITER : NFC_MODE_NONE;
    gboolean requested = self->mode_pending;
    self->mode_pending = FALSE;
    nfc_adapter_mode_notify(&self->parent, mode, requested);
}

static gboolean power_request(NfcAdapter* adapter, gboolean on)
{
    LinuxAdapter* self = (LinuxAdapter*)adapter;
    if (self->stopped) return FALSE;
    self->want_power = on;
    self->power_pending = TRUE;
    self->failed = FALSE;
    self->generation++;
    reconcile(self);
    return TRUE;
}
static void cancel_power(NfcAdapter* adapter)
{
    LinuxAdapter* self = (LinuxAdapter*)adapter;
    self->power_pending = FALSE;
    self->want_power = adapter->powered;
    self->generation++;
}
static gboolean mode_request(NfcAdapter* adapter, NFC_MODE mode)
{
    LinuxAdapter* self = (LinuxAdapter*)adapter;
    if (self->stopped || (mode & ~NFC_MODE_READER_WRITER)) return FALSE;
    self->want_reader = !!(mode & NFC_MODE_READER_WRITER);
    self->mode_pending = TRUE;
    self->failed = FALSE;
    self->generation++;
    reconcile(self);
    return TRUE;
}
static void cancel_mode(NfcAdapter* adapter)
{
    LinuxAdapter* self = (LinuxAdapter*)adapter;
    self->mode_pending = FALSE;
    self->want_reader = !!(adapter->mode & NFC_MODE_READER_WRITER);
    self->generation++;
}
static NFC_TECHNOLOGY supported_techs(NfcAdapter* adapter)
{
    LinuxAdapter* self = (LinuxAdapter*)adapter;
    return (self->protocols & (NFC_PROTO_MIFARE_MASK | NFC_PROTO_ISO14443_MASK) ?
        NFC_TECHNOLOGY_A : 0) |
        (self->protocols & NFC_PROTO_ISO14443_B_MASK ? NFC_TECHNOLOGY_B : 0);
}
static void allowed_techs(NfcAdapter* adapter, NFC_TECHNOLOGY techs)
{
    LinuxAdapter* self = (LinuxAdapter*)adapter;
    if (self->allowed != (guint)techs) {
        self->allowed = techs;
        self->generation++;
        self->repoll = TRUE;
        /* Force polling to stop before applying a different protocol mask. */
        clear_target(self);
        reconcile(self);
    }
}
static void adapter_dispose(GObject* object)
{
    LinuxAdapter* self = (LinuxAdapter*)object;
    self->stopped = TRUE;
    if (self->removed_handler) {
        g_signal_handler_disconnect(self, self->removed_handler);
        self->removed_handler = 0;
    }
    clear_target(self);
    G_OBJECT_CLASS(linux_adapter_parent_class)->dispose(object);
}
static void adapter_finalize(GObject* object)
{
    LinuxAdapter* self = (LinuxAdapter*)object;
    linux_nfc_control_free(self->control);
    if (self->plugin) nfc_plugin_unref(self->plugin);
    G_OBJECT_CLASS(linux_adapter_parent_class)->finalize(object);
}
static void linux_adapter_init(LinuxAdapter* self)
{
    self->allowed = NFC_TECHNOLOGY_A | NFC_TECHNOLOGY_B;
    self->control = linux_nfc_control_new();
    self->removed_handler = nfc_adapter_add_tag_removed_handler(&self->parent,
        removed, NULL);
}
static void linux_adapter_class_init(LinuxAdapterClass* klass)
{
    G_OBJECT_CLASS(klass)->dispose = adapter_dispose;
    G_OBJECT_CLASS(klass)->finalize = adapter_finalize;
    klass->submit_power_request = power_request;
    klass->cancel_power_request = cancel_power;
    klass->submit_mode_request = mode_request;
    klass->cancel_mode_request = cancel_mode;
    klass->get_supported_techs = supported_techs;
    klass->set_allowed_techs = allowed_techs;
}

typedef struct {
    NfcPlugin parent;
    NfcManager* manager;
    GHashTable* adapters;
    struct nl_sock* events;
    guint watch;
    gboolean enumerating, rescan;
} LinuxPlugin;
typedef NfcPluginClass LinuxPluginClass;
G_DEFINE_TYPE(LinuxPlugin, linux_plugin, NFC_TYPE_PLUGIN)

static void enumerate(LinuxPlugin* self);
static void detach(LinuxPlugin* self, guint32 index, gboolean shutdown)
{
    LinuxAdapter* adapter = g_hash_table_lookup(self->adapters, GUINT_TO_POINTER(index));
    if (adapter) {
        adapter->stopped = TRUE;
        adapter->generation++;
        adapter->want_reader = adapter->want_power = FALSE;
        adapter->discover = FALSE;
        adapter->failed = !shutdown;
        clear_target(adapter);
        if (shutdown) reconcile(adapter);
        nfc_manager_remove_adapter(self->manager, adapter->parent.name);
        g_hash_table_remove(self->adapters, GUINT_TO_POINTER(index));
    }
}
static void enumerated(GObject* object, GAsyncResult* result, gpointer data)
{
    LinuxPlugin* self = (LinuxPlugin*)object;
    Job* job = g_task_get_task_data(G_TASK(result));
    (void)data;
    self->enumerating = FALSE;
    if (!self->manager) return;
    if (self->rescan) { self->rescan = FALSE; enumerate(self); return; }
    if (job->status) {
        g_warning("linux-nfc: enumeration: %s", g_strerror(-job->status));
        return;
    }
    for (guint i = 0; i < job->records->len; i++) {
        LinuxDevice* dev = &g_array_index(job->records, LinuxDevice, i);
        if (!(dev->protocols & READER_PROTOCOLS) ||
            g_hash_table_contains(self->adapters, GUINT_TO_POINTER(dev->index))) continue;
        LinuxAdapter* adapter = g_object_new(linux_adapter_get_type(), NULL);
        adapter->index = dev->index;
        adapter->plugin = nfc_plugin_ref(&self->parent);
        adapter->protocols = dev->protocols & READER_PROTOCOLS;
        adapter->parent.supported_modes = NFC_MODE_READER_WRITER;
        adapter->parent.supported_protocols =
            (dev->protocols & NFC_PROTO_MIFARE_MASK ? NFC_PROTOCOL_T2_TAG : 0) |
            (dev->protocols & NFC_PROTO_ISO14443_MASK ? NFC_PROTOCOL_T4A_TAG : 0) |
            (dev->protocols & NFC_PROTO_ISO14443_B_MASK ? NFC_PROTOCOL_T4B_TAG : 0);
        adapter->parent.supported_tags = dev->protocols & NFC_PROTO_MIFARE_MASK ?
            NFC_TAG_TYPE_MIFARE_ULTRALIGHT : NFC_TAG_TYPE_UNKNOWN;
        adapter->want_power = dev->powered;
        nfc_adapter_power_notify(&adapter->parent, dev->powered, FALSE);
        g_hash_table_insert(self->adapters, GUINT_TO_POINTER(dev->index), adapter);
        nfc_manager_add_adapter(self->manager, &adapter->parent);
    }
}
static void enumerate(LinuxPlugin* self)
{
    if (self->enumerating) { self->rescan = TRUE; return; }
    Job* job = g_new0(Job, 1);
    GTask* task = g_task_new(self, NULL, enumerated, NULL);
    job->command = NFC_CMD_GET_DEVICE;
    job->records = g_array_new(FALSE, FALSE, sizeof(LinuxDevice));
    job->fd = -1;
    self->enumerating = TRUE;
    g_task_set_task_data(task, job, job_free);
    g_task_run_in_thread(task, worker);
    g_object_unref(task);
}
static int event(struct nl_msg* msg, void* data)
{
    LinuxPlugin* self = data;
    guint command;
    guint32 index, target;
    if (!linux_nfc_parse_event(msg, &command, &index, &target)) return NL_SKIP;
    LinuxAdapter* adapter = g_hash_table_lookup(self->adapters, GUINT_TO_POINTER(index));
    switch (command) {
    case NFC_EVENT_DEVICE_ADDED: enumerate(self); break;
    case NFC_EVENT_DEVICE_REMOVED:
        if (self->enumerating) self->rescan = TRUE;
        detach(self, index, FALSE);
        break;
    case NFC_EVENT_TARGETS_FOUND:
        if (adapter) {
            adapter->polling = FALSE;
            adapter->discover = TRUE;
            reconcile(adapter);
        }
        break;
    case NFC_EVENT_TARGET_LOST:
        if (adapter) {
            adapter->generation++;
            if (adapter->target && adapter->target_index == target)
                clear_target(adapter);
            reconcile(adapter);
        }
        break;
    }
    return NL_OK;
}
static gboolean events_ready(gint fd, GIOCondition condition, gpointer data)
{
    LinuxPlugin* self = data;
    (void)fd;
    int rc = nl_recvmsgs_default(self->events);
    if ((condition & (G_IO_ERR | G_IO_HUP | G_IO_NVAL)) ||
        (rc < 0 && rc != -NLE_AGAIN)) {
        g_warning("linux-nfc: event socket failed; restart nfcd to reconnect");
        self->watch = 0;
        /* Do not leave apparently usable adapters after losing events. */
        GList* keys = g_hash_table_get_keys(self->adapters);
        for (GList* p = keys; p; p = p->next) detach(self, GPOINTER_TO_UINT(p->data), TRUE);
        g_list_free(keys);
        return G_SOURCE_REMOVE;
    }
    return G_SOURCE_CONTINUE;
}
static gboolean start(NfcPlugin* plugin, NfcManager* manager)
{
    LinuxPlugin* self = (LinuxPlugin*)plugin;
    self->events = linux_nfc_events(event, self);
    if (!self->events) {
        g_warning("linux-nfc: Linux NFC event family unavailable");
        return FALSE;
    }
    self->manager = manager;
    self->watch = g_unix_fd_add(nl_socket_get_fd(self->events),
        G_IO_IN | G_IO_ERR | G_IO_HUP | G_IO_NVAL, events_ready, self);
    enumerate(self);
    return TRUE;
}
static void stop(NfcPlugin* plugin)
{
    LinuxPlugin* self = (LinuxPlugin*)plugin;
    if (self->watch) { g_source_remove(self->watch); self->watch = 0; }
    if (self->events) { nl_socket_free(self->events); self->events = NULL; }
    GList* keys = g_hash_table_get_keys(self->adapters);
    for (GList* p = keys; p; p = p->next) detach(self, GPOINTER_TO_UINT(p->data), TRUE);
    g_list_free(keys);
    self->manager = NULL;
}
static void plugin_finalize(GObject* object)
{
    LinuxPlugin* self = (LinuxPlugin*)object;
    g_hash_table_unref(self->adapters);
    G_OBJECT_CLASS(linux_plugin_parent_class)->finalize(object);
}
static void linux_plugin_init(LinuxPlugin* self)
{
    self->adapters = g_hash_table_new_full(g_direct_hash, g_direct_equal,
        NULL, g_object_unref);
}
static void linux_plugin_class_init(LinuxPluginClass* klass)
{
    G_OBJECT_CLASS(klass)->finalize = plugin_finalize;
    klass->start = start;
    klass->stop = stop;
}
static NfcPlugin* create(void) { return g_object_new(linux_plugin_get_type(), NULL); }
NFC_PLUGIN_DEFINE(linux, "Linux kernel NFC reader backend", create)
