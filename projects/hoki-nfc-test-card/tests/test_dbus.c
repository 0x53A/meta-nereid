/* SPDX-License-Identifier: BSD-3-Clause */
#include <gio/gio.h>
#include <signal.h>

#define SERVICE "org.sailfishos.nfc.daemon"
#define APP_PATH "/org/hoki/NfcTestCard"
#define APP_IFACE "org.sailfishos.nfc.LocalHostApp"
static const char xml[] =
    "<node><interface name='org.sailfishos.nfc.Daemon'>"
    "<method name='GetAdapters'><arg type='ao' direction='out'/></method>"
    "<method name='RegisterLocalHostApp'><arg type='o' direction='in'/>"
    "<arg type='s' direction='in'/><arg type='ay' direction='in'/><arg type='u' direction='in'/></method>"
    "<method name='UnregisterLocalHostApp'><arg type='o' direction='in'/></method>"
    "<method name='RequestMode'><arg type='u' direction='in'/><arg type='u' direction='in'/>"
    "<arg type='u' direction='out'/></method>"
    "<method name='ReleaseMode'><arg type='u' direction='in'/></method>"
    "</interface><interface name='org.sailfishos.nfc.Adapter'>"
    "<method name='GetSupportedModes'><arg type='u' direction='out'/></method>"
    "</interface></node>";
typedef struct {
    GTestDBus* bus;
    GDBusConnection* daemon;
    GSubprocess* child;
    gchar* client;
    guint modes, request;
    gboolean registered, unregistered, released, exited;
    guint exports[2];
} Test;
static void handle(GDBusConnection* conn, const gchar* sender, const gchar* path,
    const gchar* iface, const gchar* name, GVariant* args,
    GDBusMethodInvocation* invocation, gpointer data)
{
    Test* test = data;
    (void)conn; (void)path; (void)iface;
    if (!g_strcmp0(name, "GetAdapters")) {
        const char* paths[] = { "/nfc0", NULL };
        g_dbus_method_invocation_return_value(invocation, g_variant_new("(^ao)", paths));
    } else if (!g_strcmp0(name, "GetSupportedModes"))
        g_dbus_method_invocation_return_value(invocation, g_variant_new("(u)", test->modes));
    else if (!g_strcmp0(name, "RegisterLocalHostApp")) {
        const char *app_path, *app_name;
        GVariant* aid;
        guint flags;
        g_variant_get(args, "(&o&s@ayu)", &app_path, &app_name, &aid, &flags);
        const guint8 expected[] = { 0xd2, 0x76, 0, 0, 0x85, 1, 1 };
        gsize len;
        const void* bytes = g_variant_get_fixed_array(aid, &len, 1);
        g_assert_cmpmem(bytes, len, expected, sizeof(expected));
        g_assert_cmpstr(app_path, ==, APP_PATH);
        g_assert_cmpstr(app_name, ==, "Hoki NFC test card");
        g_assert_cmpuint(flags, ==, 0);
        g_variant_unref(aid);
        test->client = g_strdup(sender);
        test->registered = TRUE;
        g_dbus_method_invocation_return_value(invocation, NULL);
    } else if (!g_strcmp0(name, "RequestMode")) {
        guint enabled, disabled;
        g_variant_get(args, "(uu)", &enabled, &disabled);
        g_assert_cmpuint(enabled, ==, 8);
        g_assert_cmpuint(disabled, ==, 7);
        test->request = 42;
        g_dbus_method_invocation_return_value(invocation, g_variant_new("(u)", test->request));
    } else if (!g_strcmp0(name, "ReleaseMode")) {
        guint id;
        g_variant_get(args, "(u)", &id);
        g_assert_cmpuint(id, ==, test->request);
        test->released = TRUE;
        g_dbus_method_invocation_return_value(invocation, NULL);
    } else {
        g_assert_cmpstr(name, ==, "UnregisterLocalHostApp");
        test->unregistered = TRUE;
        g_dbus_method_invocation_return_value(invocation, NULL);
    }
}
static const GDBusInterfaceVTable vtable = { .method_call = handle };
static void wait_flag(gboolean* flag)
{
    gint64 deadline = g_get_monotonic_time() + 5 * G_TIME_SPAN_SECOND;
    while (!*flag && g_get_monotonic_time() < deadline) {
        while (g_main_context_iteration(NULL, FALSE));
        g_usleep(1000);
    }
    g_assert_true(*flag);
}
static void exited(GObject* object, GAsyncResult* result, gpointer data)
{
    GError* error = NULL;
    g_subprocess_wait_finish(G_SUBPROCESS(object), result, &error);
    g_assert_no_error(error);
    ((Test*)data)->exited = TRUE;
}
static void setup(Test* test, guint modes)
{
    GError* error = NULL;
    *test = (Test) { .modes = modes };
    test->bus = g_test_dbus_new(G_TEST_DBUS_NONE);
    g_test_dbus_up(test->bus);
    test->daemon = g_dbus_connection_new_for_address_sync(g_test_dbus_get_bus_address(test->bus),
        G_DBUS_CONNECTION_FLAGS_AUTHENTICATION_CLIENT | G_DBUS_CONNECTION_FLAGS_MESSAGE_BUS_CONNECTION,
        NULL, NULL, &error);
    g_assert_no_error(error);
    GVariant* reply = g_dbus_connection_call_sync(test->daemon, "org.freedesktop.DBus",
        "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
        g_variant_new("(su)", SERVICE, 0u), G_VARIANT_TYPE("(u)"), 0, 3000, NULL, &error);
    g_assert_no_error(error);
    g_variant_unref(reply);
    GDBusNodeInfo* info = g_dbus_node_info_new_for_xml(xml, &error);
    g_assert_no_error(error);
    test->exports[0] = g_dbus_connection_register_object(test->daemon, "/", info->interfaces[0],
        &vtable, test, NULL, &error);
    g_assert_no_error(error);
    test->exports[1] = g_dbus_connection_register_object(test->daemon, "/nfc0", info->interfaces[1],
        &vtable, test, NULL, &error);
    g_assert_no_error(error);
    g_dbus_node_info_unref(info);
    test->child = g_subprocess_new(G_SUBPROCESS_FLAGS_STDOUT_PIPE | G_SUBPROCESS_FLAGS_STDERR_PIPE,
        &error, "./build/hoki-nfc-test-card", "--session", NULL);
    g_assert_no_error(error);
    g_subprocess_wait_async(test->child, NULL, exited, test);
}
static void teardown(Test* test)
{
    if (!test->exited) {
        g_subprocess_send_signal(test->child, SIGTERM);
        wait_flag(&test->exited);
    }
    g_object_unref(test->child);
    for (guint i = 0; i < 2; i++)
        g_dbus_connection_unregister_object(test->daemon, test->exports[i]);
    g_dbus_connection_close_sync(test->daemon, NULL, NULL);
    g_object_unref(test->daemon);
    g_test_dbus_down(test->bus);
    g_object_unref(test->bus);
    g_free(test->client);
}
typedef struct { gboolean done; GVariant* value; GError* error; } Response;
static void response(GObject* object, GAsyncResult* result, gpointer data)
{
    Response* reply = data;
    reply->value = g_dbus_connection_call_finish(G_DBUS_CONNECTION(object), result, &reply->error);
    reply->done = TRUE;
}
static GVariant* invoke(Test* test, GDBusConnection* connection, const char* method,
    GVariant* args, gboolean success)
{
    Response reply = { 0 };
    g_dbus_connection_call(connection, test->client, APP_PATH, APP_IFACE, method,
        args, NULL, 0, 3000, NULL, response, &reply);
    wait_flag(&reply.done);
    if (success) g_assert_no_error(reply.error);
    else { g_assert_nonnull(reply.error); g_assert_null(reply.value); g_clear_error(&reply.error); }
    return reply.value;
}
static void lifecycle(Test* test, const char* method)
{
    GVariant* result = invoke(test, test->daemon, method, g_variant_new("(o)", "/host0"), TRUE);
    g_variant_unref(result);
}
static GVariant* process(Test* test, guint8 ins, guint8 p2, const guint8* data, gsize size, guint le)
{
    return invoke(test, test->daemon, "Process", g_variant_new("(oyyyy@ayu)", "/host0",
        0, ins, 0, p2, g_variant_new_fixed_array(G_VARIANT_TYPE_BYTE, data, size, 1), le), TRUE);
}
static void expect_status(GVariant* value, guint16 expected)
{
    GVariant* bytes;
    guint8 sw1, sw2;
    guint id;
    g_variant_get(value, "(@ayyyu)", &bytes, &sw1, &sw2, &id);
    g_assert_cmphex(((guint)sw1 << 8) | sw2, ==, expected);
    g_assert_cmpuint(id, ==, 0);
    g_variant_unref(bytes);
    g_variant_unref(value);
}
static void unsupported(void)
{
    Test test;
    setup(&test, 2);
    wait_flag(&test.exited);
    g_assert_false(test.registered);
    g_assert_cmpint(g_subprocess_get_exit_status(test.child), ==, 3);
    teardown(&test);
}
static void wire_protocol(void)
{
    Test test;
    setup(&test, 8);
    wait_flag(&test.registered);
    lifecycle(&test, "Start");
    lifecycle(&test, "Select");
    const guint8 file[] = { 0xe1, 0x04 };
    expect_status(process(&test, 0xa4, 0x0c, file, 2, 0), 0x9000);
    GVariant* nlen = process(&test, 0xb0, 0, NULL, 0, 2);
    GVariant* bytes;
    guint8 sw1, sw2;
    guint id;
    g_variant_get(nlen, "(@ayyyu)", &bytes, &sw1, &sw2, &id);
    gsize len;
    const guint8* raw = g_variant_get_fixed_array(bytes, &len, 1);
    g_assert_cmpuint(len, ==, 2);
    g_assert_cmpuint(raw[0], ==, 0);
    g_assert_cmpuint(raw[1], ==, 25);
    g_variant_unref(bytes);
    expect_status(nlen, 0x9000);
    expect_status(process(&test, 0xd6, 0, file, 2, 0), 0x6982);
    lifecycle(&test, "Restart");
    expect_status(process(&test, 0xb0, 0, NULL, 0, 2), 0x6985);
    lifecycle(&test, "Stop");
    teardown(&test);
    g_assert_true(test.released);
    g_assert_true(test.unregistered);
}
static void sender_check(void)
{
    Test test;
    setup(&test, 8);
    wait_flag(&test.registered);
    GError* error = NULL;
    GDBusConnection* stranger = g_dbus_connection_new_for_address_sync(
        g_test_dbus_get_bus_address(test.bus), G_DBUS_CONNECTION_FLAGS_AUTHENTICATION_CLIENT |
        G_DBUS_CONNECTION_FLAGS_MESSAGE_BUS_CONNECTION, NULL, NULL, &error);
    g_assert_no_error(error);
    g_assert_null(invoke(&test, stranger, "Start", g_variant_new("(o)", "/host0"), FALSE));
    g_dbus_connection_close_sync(stranger, NULL, NULL);
    g_object_unref(stranger);
    teardown(&test);
}
static void daemon_loss(void)
{
    Test test;
    setup(&test, 8);
    wait_flag(&test.registered);
    g_dbus_connection_close_sync(test.daemon, NULL, NULL);
    wait_flag(&test.exited);
    g_assert_cmpint(g_subprocess_get_exit_status(test.child), ==, 2);
    teardown(&test);
}
int main(int argc, char** argv)
{
    g_test_init(&argc, &argv, NULL);
    g_test_add_func("/dbus/unsupported", unsupported);
    g_test_add_func("/dbus/wire-protocol", wire_protocol);
    g_test_add_func("/dbus/sender-check", sender_check);
    g_test_add_func("/dbus/daemon-loss", daemon_loss);
    return g_test_run();
}
