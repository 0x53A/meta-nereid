/* SPDX-License-Identifier: BSD-3-Clause */
#include "linux_target.h"
#include <nfc_target_impl.h>
#include <gutil_log.h>
#include <linux/nfc.h>
#include <sys/socket.h>
#include <unistd.h>
#include <errno.h>

GLogModule nfc_core_log = { .name = "test" };
typedef struct {
    guint completions;
    NFC_TRANSMIT_STATUS status;
    GBytes* response;
} Result;
static void completed(NfcTarget* target, NFC_TRANSMIT_STATUS status,
    const void* data, guint len, void* user_data)
{
    Result* result = user_data;
    (void)target;
    result->completions++;
    result->status = status;
    if (result->response) g_bytes_unref(result->response);
    result->response = g_bytes_new(data, len);
}
static void wait_for(guint* value)
{
    gint64 deadline = g_get_monotonic_time() + G_TIME_SPAN_SECOND;
    while (!*value && g_get_monotonic_time() < deadline) {
        while (g_main_context_iteration(NULL, FALSE));
        g_usleep(1000);
    }
    g_assert_cmpuint(*value, >, 0);
}
static NfcTarget* pair(int* peer)
{
    int sockets[2];
    g_assert_cmpint(socketpair(AF_UNIX, SOCK_SEQPACKET | SOCK_NONBLOCK, 0, sockets), ==, 0);
    *peer = sockets[1];
    return linux_target_new(sockets[0], NFC_PROTO_MIFARE, NULL);
}
static void exchange(void)
{
    int peer;
    NfcTarget* target = pair(&peer);
    Result result = { 0 };
    const guint8 command[] = { 1, 2 }, reply[] = { 0, 3, 4 };
    guint8 received[10];
    g_assert_cmpuint(nfc_target_transmit(target, command, sizeof(command), NULL,
        completed, NULL, &result), !=, 0);
    g_assert_cmpint(recv(peer, received, sizeof(received), 0), ==, sizeof(command));
    g_assert_cmpmem(received, sizeof(command), command, sizeof(command));
    g_assert_cmpint(send(peer, reply, sizeof(reply), 0), ==, sizeof(reply));
    wait_for(&result.completions);
    g_assert_cmpint(result.status, ==, NFC_TRANSMIT_STATUS_OK);
    gsize len;
    const void* bytes = g_bytes_get_data(result.response, &len);
    g_assert_cmpmem(bytes, len, reply + 1, sizeof(reply) - 1);
    g_bytes_unref(result.response);
    linux_target_close(target);
    nfc_target_unref(target);
    close(peer);
}
static void timeout_closes_connection(void)
{
    int peer;
    NfcTarget* target = pair(&peer);
    Result result = { 0 };
    guint8 command = 1;
    nfc_target_set_transmit_timeout(target, 10);
    g_assert_cmpuint(nfc_target_transmit(target, &command, 1, NULL,
        completed, NULL, &result), !=, 0);
    wait_for(&result.completions);
    g_assert_cmpint(result.status, ==, NFC_TRANSMIT_STATUS_TIMEOUT);
    while (g_main_context_iteration(NULL, FALSE));
    g_assert_false(target->present);
    /* The old peer cannot inject a response into a subsequent request. */
    g_assert_cmpint(send(peer, &command, 1, MSG_NOSIGNAL), ==, -1);
    g_assert_cmpint(errno, ==, EPIPE);
    g_assert_cmpuint(nfc_target_transmit(target, &command, 1, NULL,
        completed, NULL, &result), ==, 0);
    g_bytes_unref(result.response);
    nfc_target_unref(target);
    close(peer);
}
static void kernel_error(void)
{
    int peer;
    NfcTarget* target = pair(&peer);
    Result result = { 0 };
    guint8 command = 1, error[] = { 0xff };
    nfc_target_transmit(target, &command, 1, NULL, completed, NULL, &result);
    send(peer, error, sizeof(error), 0);
    wait_for(&result.completions);
    g_assert_cmpint(result.status, ==, NFC_TRANSMIT_STATUS_ERROR);
    while (g_main_context_iteration(NULL, FALSE));
    g_assert_false(target->present);
    g_bytes_unref(result.response);
    nfc_target_unref(target);
    close(peer);
}
static void peer_disconnect(void)
{
    int peer;
    NfcTarget* target = pair(&peer);
    Result result = { 0 };
    guint8 command = 1;
    nfc_target_transmit(target, &command, 1, NULL, completed, NULL, &result);
    close(peer);
    wait_for(&result.completions);
    g_assert_cmpint(result.status, ==, NFC_TRANSMIT_STATUS_ERROR);
    while (g_main_context_iteration(NULL, FALSE));
    g_assert_false(target->present);
    g_bytes_unref(result.response);
    nfc_target_unref(target);
}
int main(int argc, char** argv)
{
    g_test_init(&argc, &argv, NULL);
    g_test_add_func("/target/exchange", exchange);
    g_test_add_func("/target/timeout", timeout_closes_connection);
    g_test_add_func("/target/kernel-error", kernel_error);
    g_test_add_func("/target/disconnect", peer_disconnect);
    return g_test_run();
}
