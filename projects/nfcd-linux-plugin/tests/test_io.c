/* SPDX-License-Identifier: BSD-3-Clause */
#include "linux_io.h"
#include <netlink/genl/genl.h>
#include <errno.h>

static struct nl_msg* message(void)
{
    struct nl_msg* msg = nlmsg_alloc();
    genlmsg_put(msg, 0, 1, 25, 0, 0, NFC_CMD_GET_TARGET, NFC_GENL_VERSION);
    return msg;
}
static void missing(void)
{
    LinuxTag tag;
    LinuxDevice dev;
    struct nl_msg* msg = message();
    g_assert_false(linux_nfc_parse_tag(msg, &tag));
    g_assert_false(linux_nfc_parse_device(msg, &dev));
    nla_put_u32(msg, NFC_ATTR_TARGET_INDEX, 0);
    nla_put_u32(msg, NFC_ATTR_PROTOCOLS, NFC_PROTO_MIFARE_MASK);
    g_assert_true(linux_nfc_parse_tag(msg, &tag));
    g_assert_cmpuint(tag.index, ==, 0);
    g_assert_cmpuint(linux_nfc_protocol(&tag), ==, 0);
    nlmsg_free(msg);
}
static void oversized_uid(void)
{
    LinuxTag tag;
    guint8 uid[11] = { 0 };
    struct nl_msg* msg = message();
    nla_put_u32(msg, NFC_ATTR_TARGET_INDEX, 1);
    nla_put_u32(msg, NFC_ATTR_PROTOCOLS, NFC_PROTO_MIFARE_MASK);
    nla_put(msg, NFC_ATTR_TARGET_NFCID1, sizeof(uid), uid);
    g_assert_false(linux_nfc_parse_tag(msg, &tag));
    nlmsg_free(msg);
}
static void wrong_width(void)
{
    LinuxTag tag;
    struct nl_msg* msg = message();
    nla_put_u8(msg, NFC_ATTR_TARGET_INDEX, 1);
    nla_put_u32(msg, NFC_ATTR_PROTOCOLS, NFC_PROTO_MIFARE_MASK);
    g_assert_false(linux_nfc_parse_tag(msg, &tag));
    nlmsg_free(msg);
}
static void protocol_selection(void)
{
    LinuxTag tag = { .protocols = NFC_PROTO_MIFARE_MASK, .uid_len = 7 };
    g_assert_cmpuint(linux_nfc_protocol(&tag), ==, NFC_PROTO_MIFARE);
    tag.sak = 8;
    g_assert_cmpuint(linux_nfc_protocol(&tag), ==, 0); /* Classic isn't T2. */
    tag.protocols |= NFC_PROTO_ISO14443_MASK;
    g_assert_cmpuint(linux_nfc_protocol(&tag), ==, NFC_PROTO_ISO14443);
    tag.protocols = NFC_PROTO_ISO14443_B_MASK;
    g_assert_cmpuint(linux_nfc_protocol(&tag), ==, NFC_PROTO_ISO14443_B);
    tag.protocols = NFC_PROTO_NFC_DEP_MASK;
    g_assert_cmpuint(linux_nfc_protocol(&tag), ==, 0);
}
static void payload(void)
{
    const guint8 valid[] = { 0, 1, 2 }, failed[] = { 1, 2 };
    const guint8* bytes;
    gsize len;
    g_assert_cmpint(linux_nfc_payload(valid, sizeof(valid), &bytes, &len), ==, 0);
    g_assert_cmpmem(bytes, len, valid + 1, 2);
    g_assert_cmpint(linux_nfc_payload(failed, sizeof(failed), &bytes, &len), ==, -EIO);
    g_assert_null(bytes);
    g_assert_cmpint(linux_nfc_payload(NULL, 0, &bytes, &len), ==, -EPROTO);
    g_assert_cmpint(linux_nfc_payload(valid, 1, &bytes, &len), ==, 0);
    g_assert_cmpuint(len, ==, 0);
}
int main(int argc, char** argv)
{
    g_test_init(&argc, &argv, NULL);
    g_test_add_func("/linux-nfc/missing-attributes", missing);
    g_test_add_func("/linux-nfc/oversized-uid", oversized_uid);
    g_test_add_func("/linux-nfc/wrong-width", wrong_width);
    g_test_add_func("/linux-nfc/protocol-selection", protocol_selection);
    g_test_add_func("/linux-nfc/payload", payload);
    return g_test_run();
}
