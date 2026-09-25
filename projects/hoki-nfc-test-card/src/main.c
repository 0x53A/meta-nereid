/* SPDX-License-Identifier: BSD-3-Clause */
#include "card.h"
#include <gio/gio.h>
#include <glib-unix.h>
#include <signal.h>

#define DAEMON "org.sailfishos.nfc.daemon"
#define DAEMON_IFACE "org.sailfishos.nfc.Daemon"
#define APP_IFACE "org.sailfishos.nfc.LocalHostApp"
#define APP_PATH "/org/hoki/NfcTestCard"
#define HCE_MODE 8u

static const char xml[] =
    "<node><interface name='" APP_IFACE "'>"
    "<method name='GetInterfaceVersion'><arg type='i' direction='out'/></method>"
    "<method name='Start'><arg type='o' direction='in'/></method>"
    "<method name='Restart'><arg type='o' direction='in'/></method>"
    "<method name='Stop'><arg type='o' direction='in'/></method>"
    "<method name='Select'><arg type='o' direction='in'/></method>"
    "<method name='ImplicitSelect'><arg type='o' direction='in'/></method>"
    "<method name='Deselect'><arg type='o' direction='in'/></method>"
    "<method name='Process'><arg type='o' direction='in'/>"
    "<arg type='y' direction='in'/><arg type='y' direction='in'/>"
    "<arg type='y' direction='in'/><arg type='y' direction='in'/>"
    "<arg type='ay' direction='in'/><arg type='u' direction='in'/>"
    "<arg type='ay' direction='out'/><arg type='y' direction='out'/>"
    "<arg type='y' direction='out'/><arg type='u' direction='out'/></method>"
    "<method name='ResponseStatus'><arg type='u' direction='in'/>"
    "<arg type='b' direction='in'/></method>"
    "</interface></node>";

typedef struct {
    GDBusConnection* bus;
    gchar* owner;
    GHashTable* sessions;
    GMainLoop* loop;
    int result;
} App;

static GVariant* call(App* app, const char* path, const char* iface,
    const char* method, GVariant* args, const GVariantType* type, GError** error)
{
    return g_dbus_connection_call_sync(app->bus, app->owner, path, iface, method,
        args, type, G_DBUS_CALL_FLAGS_NO_AUTO_START, 3000, NULL, error);
}
static void reject(GDBusMethodInvocation* invocation, const char* message)
{
    g_dbus_method_invocation_return_dbus_error(invocation,
        "org.hoki.NfcTestCard.Error", message);
}
static void method(GDBusConnection* bus, const gchar* sender, const gchar* path,
    const gchar* interface, const gchar* name, GVariant* args,
    GDBusMethodInvocation* invocation, gpointer data)
{
    App* app = data;
    const char* host = NULL;
    (void)bus; (void)path; (void)interface;
    if (g_strcmp0(sender, app->owner)) { reject(invocation, "Only nfcd may call this object"); return; }
    if (!g_strcmp0(name, "GetInterfaceVersion")) {
        g_dbus_method_invocation_return_value(invocation, g_variant_new("(i)", 1));
        return;
    }
    if (!g_strcmp0(name, "ResponseStatus")) {
        g_dbus_method_invocation_return_value(invocation, NULL);
        return;
    }
    if (!g_strcmp0(name, "Process")) {
        guint8 cla, ins, p1, p2;
        guint le;
        GVariant* bytes;
        g_variant_get(args, "(&oyyyy@ayu)", &host, &cla, &ins, &p1, &p2, &bytes, &le);
        CardSession* session = g_hash_table_lookup(app->sessions, host);
        if (session) {
            gsize size;
            const guint8* data_bytes = g_variant_get_fixed_array(bytes, &size, 1);
            CardReply reply = card_process(session, cla, ins, p1, p2, data_bytes, size, le);
            GVariant* response = g_variant_new_fixed_array(G_VARIANT_TYPE_BYTE,
                reply.data, reply.size, 1);
            g_dbus_method_invocation_return_value(invocation,
                g_variant_new("(@ayyyu)", response, reply.status >> 8,
                    reply.status & 0xff, 0u));
        } else reject(invocation, "Unknown host session");
        g_variant_unref(bytes);
        return;
    }
    g_variant_get(args, "(&o)", &host);
    if (!g_strcmp0(name, "Start")) {
        if (g_hash_table_size(app->sessions) >= 8 &&
            !g_hash_table_contains(app->sessions, host)) {
            reject(invocation, "Too many host sessions"); return;
        }
        g_hash_table_replace(app->sessions, g_strdup(host), g_new0(CardSession, 1));
    } else if (!g_strcmp0(name, "Stop")) g_hash_table_remove(app->sessions, host);
    else {
        CardSession* session = g_hash_table_lookup(app->sessions, host);
        if (!session) { reject(invocation, "Unknown host session"); return; }
        if (!g_strcmp0(name, "Restart") || !g_strcmp0(name, "Deselect")) card_session_reset(session);
        else if (!g_strcmp0(name, "Select")) {
            card_session_reset(session);
            session->selected = TRUE;
        } else { reject(invocation, "Explicit NDEF application selection required"); return; }
    }
    g_dbus_method_invocation_return_value(invocation, NULL);
}
static const GDBusInterfaceVTable vtable = { .method_call = method };

static gboolean quit_signal(gpointer data)
{
    g_main_loop_quit(((App*)data)->loop);
    return G_SOURCE_CONTINUE;
}
static void vanished(GDBusConnection* bus, const gchar* name, gpointer data)
{
    App* app = data;
    (void)bus; (void)name;
    g_printerr("nfcd disconnected; test card stopped.\n");
    app->result = 2;
    g_main_loop_quit(app->loop);
}
static gboolean capable(App* app, GError** error)
{
    GVariant* result = call(app, "/", DAEMON_IFACE, "GetAdapters", NULL,
        G_VARIANT_TYPE("(ao)"), error);
    if (!result) return FALSE;
    gchar** paths;
    g_variant_get(result, "(^ao)", &paths);
    g_variant_unref(result);
    gboolean found = FALSE;
    for (guint i = 0; paths[i] && !found; i++) {
        result = call(app, paths[i], "org.sailfishos.nfc.Adapter",
            "GetSupportedModes", NULL, G_VARIANT_TYPE("(u)"), error);
        if (!result) break;
        guint modes;
        g_variant_get(result, "(u)", &modes);
        g_variant_unref(result);
        found = !!(modes & HCE_MODE);
    }
    g_strfreev(paths);
    if (!found && !*error) g_set_error_literal(error, G_IO_ERROR,
        G_IO_ERROR_NOT_SUPPORTED, "Card emulation unsupported: no nfcd adapter advertises it");
    return found;
}

int main(int argc, char** argv)
{
    gboolean session_bus = FALSE, check = FALSE;
    GOptionEntry options[] = {
        { "session", 0, 0, G_OPTION_ARG_NONE, &session_bus, "Use an isolated session bus for testing", NULL },
        { "check", 0, 0, G_OPTION_ARG_NONE, &check, "Check capability without registering a card", NULL },
        { NULL }
    };
    GOptionContext* context = g_option_context_new("- fixed, read-only NFC test card");
    GError* error = NULL;
    App app = { .result = 2 };
    guint exported = 0, watch = 0, sigint = 0, sigterm = 0, request_id = 0;
    gboolean registered = FALSE;
    GDBusNodeInfo* info = NULL;
    g_option_context_add_main_entries(context, options, NULL);
    if (!g_option_context_parse(context, &argc, &argv, &error)) goto out;
    if (argc != 1) { g_set_error_literal(&error, G_IO_ERROR, G_IO_ERROR_INVALID_ARGUMENT, "No card data or positional arguments are accepted"); goto out; }
    app.bus = g_bus_get_sync(session_bus ? G_BUS_TYPE_SESSION : G_BUS_TYPE_SYSTEM, NULL, &error);
    if (!app.bus) goto out;
    GVariant* reply = g_dbus_connection_call_sync(app.bus,
        "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
        "GetNameOwner", g_variant_new("(s)", DAEMON), G_VARIANT_TYPE("(s)"),
        G_DBUS_CALL_FLAGS_NO_AUTO_START, 3000, NULL, &error);
    if (!reply) goto out;
    g_variant_get(reply, "(s)", &app.owner);
    g_variant_unref(reply);
    if (!capable(&app, &error)) { app.result = 3; goto out; }
    if (check) { g_print("An nfcd adapter advertises card emulation; RF operation is unverified.\n"); app.result = 0; goto out; }
    app.sessions = g_hash_table_new_full(g_str_hash, g_str_equal, g_free, g_free);
    app.loop = g_main_loop_new(NULL, FALSE);
    info = g_dbus_node_info_new_for_xml(xml, &error);
    if (!info) goto out;
    exported = g_dbus_connection_register_object(app.bus, APP_PATH, info->interfaces[0],
        &vtable, &app, NULL, &error);
    if (!exported) goto out;
    reply = call(&app, "/", DAEMON_IFACE, "RegisterLocalHostApp",
        g_variant_new("(os@ayu)", APP_PATH, "Hoki NFC test card",
            g_variant_new_fixed_array(G_VARIANT_TYPE_BYTE, card_aid, sizeof(card_aid), 1), 0u),
        G_VARIANT_TYPE_UNIT, &error);
    if (!reply) goto out;
    registered = TRUE;
    g_variant_unref(reply);
    reply = call(&app, "/", DAEMON_IFACE, "RequestMode",
        g_variant_new("(uu)", HCE_MODE, 7u), G_VARIANT_TYPE("(u)"), &error);
    if (!reply) goto out;
    g_variant_get(reply, "(u)", &request_id);
    g_variant_unref(reply);
    watch = g_bus_watch_name_on_connection(app.bus, app.owner,
        G_BUS_NAME_WATCHER_FLAGS_NONE, NULL, vanished, &app, NULL);
    sigint = g_unix_signal_add(SIGINT, quit_signal, &app);
    sigterm = g_unix_signal_add(SIGTERM, quit_signal, &app);
    app.result = 0;
    g_print("Registered read-only test text: " TEST_CARD_TEXT "\nRegistration is not confirmation of an RF link. Stop with Ctrl-C.\n");
    g_main_loop_run(app.loop);
out:
    if (error) { g_printerr("%s\n", error->message); g_clear_error(&error); }
    if (watch) g_bus_unwatch_name(watch);
    if (sigint) g_source_remove(sigint);
    if (sigterm) g_source_remove(sigterm);
    if (request_id) {
        GVariant* result = call(&app, "/", DAEMON_IFACE, "ReleaseMode",
            g_variant_new("(u)", request_id), G_VARIANT_TYPE_UNIT, NULL);
        if (result) g_variant_unref(result);
    }
    if (registered) {
        GVariant* result = call(&app, "/", DAEMON_IFACE, "UnregisterLocalHostApp",
            g_variant_new("(o)", APP_PATH), G_VARIANT_TYPE_UNIT, NULL);
        if (result) g_variant_unref(result);
    }
    if (exported) g_dbus_connection_unregister_object(app.bus, exported);
    if (info) g_dbus_node_info_unref(info);
    if (app.sessions) g_hash_table_unref(app.sessions);
    if (app.loop) g_main_loop_unref(app.loop);
    g_free(app.owner);
    g_clear_object(&app.bus);
    g_option_context_free(context);
    return app.result;
}
