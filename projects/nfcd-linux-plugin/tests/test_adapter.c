/* SPDX-License-Identifier: BSD-3-Clause */
/* Run the real nfcd adapter and backend against a deterministic kernel stub. */
#include <nfc_adapter_p.h>
#define linux_nfc_command test_command
#include "../src/linux_plugin.c"
#undef linux_nfc_command
#include <netlink/genl/genl.h>

typedef struct { guint command, protocols; LinuxNfcControl* control; } Call;
static GAsyncQueue *calls, *replies;
int test_command(LinuxNfcControl* control, guint command, guint32 device,
    guint32 mask, GArray* records)
{
    (void)records;
    g_assert_cmpuint(device, ==, 0);
    Call* call = g_new0(Call, 1);
    *call = (Call) { command, mask, control };
    g_async_queue_push(calls, call);
    int* reply = g_async_queue_pop(replies);
    int result = *reply;
    g_free(reply);
    return result;
}
static Call next_call(guint command)
{
    Call* call = NULL;
    gint64 deadline = g_get_monotonic_time() + 2 * G_TIME_SPAN_SECOND;
    while (!call && g_get_monotonic_time() < deadline) {
        while (g_main_context_iteration(NULL, FALSE));
        call = g_async_queue_try_pop(calls);
        if (!call) g_usleep(1000);
    }
    g_assert_nonnull(call);
    g_assert_cmpuint(call->command, ==, command);
    Call result = *call;
    g_free(call);
    return result;
}
static void reply(int result)
{
    int* value = g_new(int, 1);
    *value = result;
    g_async_queue_push(replies, value);
}
static void settled(LinuxAdapter* adapter)
{
    gint64 deadline = g_get_monotonic_time() + 2 * G_TIME_SPAN_SECOND;
    do {
        while (g_main_context_iteration(NULL, FALSE));
        if (!adapter->busy) break;
        g_usleep(1000);
    } while (g_get_monotonic_time() < deadline);
    g_assert_false(adapter->busy);
    g_assert_cmpint(g_async_queue_length(calls), ==, 0);
}
static LinuxAdapter* new_adapter(void)
{
    calls = g_async_queue_new();
    replies = g_async_queue_new();
    LinuxAdapter* adapter = g_object_new(linux_adapter_get_type(), NULL);
    adapter->protocols = READER_PROTOCOLS;
    adapter->parent.supported_modes = NFC_MODE_READER_WRITER;
    nfc_adapter_set_enabled(&adapter->parent, TRUE);
    return adapter;
}
static void cleanup(LinuxAdapter* adapter)
{
    settled(adapter);
    g_object_unref(adapter);
    g_async_queue_unref(calls);
    g_async_queue_unref(replies);
}
static void power_up(LinuxAdapter* adapter)
{
    nfc_adapter_request_power(&adapter->parent, TRUE);
    next_call(NFC_CMD_DEV_UP);
    reply(0);
    settled(adapter);
    g_assert_true(adapter->parent.powered);
}
static void cancelled_power_up(void)
{
    LinuxAdapter* adapter = new_adapter();
    nfc_adapter_request_power(&adapter->parent, TRUE);
    Call up = next_call(NFC_CMD_DEV_UP);
    nfc_adapter_request_power(&adapter->parent, FALSE);
    reply(0);
    Call down = next_call(NFC_CMD_DEV_DOWN);
    g_assert_true(up.control == down.control);
    reply(0);
    settled(adapter);
    g_assert_false(adapter->parent.powered);
    g_assert_false(adapter->power_pending);
    cleanup(adapter);
}
static void change_tech_during_poll(void)
{
    LinuxAdapter* adapter = new_adapter();
    power_up(adapter);
    nfc_adapter_request_mode(&adapter->parent, NFC_MODE_READER_WRITER);
    Call first = next_call(NFC_CMD_START_POLL);
    g_assert_cmpuint(first.protocols, ==, READER_PROTOCOLS);
    allowed_techs(&adapter->parent, NFC_TECHNOLOGY_B);
    reply(0);
    Call stop_call = next_call(NFC_CMD_STOP_POLL);
    g_assert_true(first.control == stop_call.control);
    reply(0);
    Call second = next_call(NFC_CMD_START_POLL);
    g_assert_cmpuint(second.protocols, ==, NFC_PROTO_ISO14443_B_MASK);
    reply(0);
    settled(adapter);
    g_assert_cmpuint(adapter->parent.mode, ==, NFC_MODE_READER_WRITER);
    nfc_adapter_request_mode(&adapter->parent, NFC_MODE_NONE);
    next_call(NFC_CMD_STOP_POLL);
    reply(0);
    settled(adapter);
    cleanup(adapter);
}
static void failure_does_not_spin(void)
{
    LinuxAdapter* adapter = new_adapter();
    power_up(adapter);
    nfc_adapter_request_mode(&adapter->parent, NFC_MODE_READER_WRITER);
    next_call(NFC_CMD_START_POLL);
    g_test_expect_message(NULL, G_LOG_LEVEL_WARNING, "*operation*Device or resource busy*");
    reply(-EBUSY);
    settled(adapter);
    g_test_assert_expected_messages();
    g_assert_true(adapter->failed);
    g_assert_cmpuint(adapter->parent.mode, ==, NFC_MODE_NONE);
    cleanup(adapter);
}
static void found_before_poll_ack(void)
{
    LinuxAdapter* adapter = new_adapter();
    power_up(adapter);
    nfc_adapter_request_mode(&adapter->parent, NFC_MODE_READER_WRITER);
    next_call(NFC_CMD_START_POLL);
    LinuxPlugin plugin = { .adapters = g_hash_table_new(g_direct_hash, g_direct_equal) };
    g_hash_table_insert(plugin.adapters, GUINT_TO_POINTER(0), adapter);
    struct nl_msg* msg = nlmsg_alloc();
    genlmsg_put(msg, 0, 0, 25, 0, 0, NFC_EVENT_TARGETS_FOUND, NFC_GENL_VERSION);
    nla_put_u32(msg, NFC_ATTR_DEVICE_INDEX, 0);
    event(msg, &plugin);
    nlmsg_free(msg);
    reply(0);
    next_call(NFC_CMD_GET_TARGET);
    g_assert_false(adapter->polling);
    reply(0); /* Empty target list: no unbounded retry. */
    settled(adapter);
    g_assert_true(adapter->failed);
    g_hash_table_unref(plugin.adapters);
    cleanup(adapter);
}
static void power_off_stops_poll_first(void)
{
    LinuxAdapter* adapter = new_adapter();
    power_up(adapter);
    nfc_adapter_request_mode(&adapter->parent, NFC_MODE_READER_WRITER);
    next_call(NFC_CMD_START_POLL);
    reply(0);
    settled(adapter);
    nfc_adapter_request_power(&adapter->parent, FALSE);
    next_call(NFC_CMD_STOP_POLL);
    reply(0);
    next_call(NFC_CMD_DEV_DOWN);
    reply(0);
    settled(adapter);
    g_assert_false(adapter->parent.powered);
    g_assert_cmpuint(adapter->parent.mode, ==, NFC_MODE_NONE);
    cleanup(adapter);
}
int main(int argc, char** argv)
{
    g_test_init(&argc, &argv, NULL);
    g_test_add_func("/adapter/cancel-power-up", cancelled_power_up);
    g_test_add_func("/adapter/change-tech-during-poll", change_tech_during_poll);
    g_test_add_func("/adapter/failure-no-spin", failure_does_not_spin);
    g_test_add_func("/adapter/found-before-ack", found_before_poll_ack);
    g_test_add_func("/adapter/power-off-order", power_off_stops_poll_first);
    return g_test_run();
}
