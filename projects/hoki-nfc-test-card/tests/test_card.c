/* SPDX-License-Identifier: BSD-3-Clause */
#include "card.h"

static CardReply select_file(CardSession* s, guint8 id)
{
    guint8 data[] = { 0xe1, id };
    return card_process(s, 0, 0xa4, 0, 0x0c, data, sizeof(data), 0);
}
static void text_read(void)
{
    CardSession s = { .selected = TRUE };
    g_assert_cmphex(select_file(&s, 3).status, ==, 0x9000);
    CardReply cc = card_process(&s, 0, 0xb0, 0, 0, NULL, 0, 15);
    g_assert_cmphex(cc.status, ==, 0x9000);
    g_assert_cmpuint(cc.size, ==, 15);
    g_assert_cmpuint(cc.data[14], ==, 0xff); /* Read-only advertised. */
    guint max_size = ((guint)cc.data[11] << 8) | cc.data[12];
    g_assert_cmphex(select_file(&s, 4).status, ==, 0x9000);
    CardReply nlen = card_process(&s, 0, 0xb0, 0, 0, NULL, 0, 2);
    guint size = ((guint)nlen.data[0] << 8) | nlen.data[1];
    g_assert_cmpuint(size + 2, ==, max_size);
    CardReply rec = card_process(&s, 0, 0xb0, 0, 2, NULL, 0, size);
    g_assert_cmphex(rec.status, ==, 0x9000);
    g_assert_cmphex(rec.data[0], ==, 0xd1);
    g_assert_cmpuint(rec.data[1], ==, 1);
    g_assert_cmpuint(rec.data[2] + 4, ==, rec.size);
    g_assert_cmpmem(rec.data + 3, 4, "T\002en", 4);
    g_assert_cmpmem(rec.data + 7, rec.size - 7,
        TEST_CARD_TEXT, sizeof(TEST_CARD_TEXT) - 1);
}
static void bounds(void)
{
    CardSession s = { .selected = TRUE };
    select_file(&s, 4);
    g_assert_cmphex(card_process(&s, 0, 0xb0, 0xff, 0xff, NULL, 0, 1).status, ==, 0x6a86);
    g_assert_cmphex(card_process(&s, 0, 0xb0, 1, 0, NULL, 0, 1).status, ==, 0x6b00);
    g_assert_cmphex(card_process(&s, 0, 0xb0, 0, 0, NULL, 0, 256).status, ==, 0x6c80);
    g_assert_cmphex(card_process(&s, 0, 0xb0, 0, 0, NULL, 0, 0).status, ==, 0x6700);
    CardReply end = card_process(&s, 0, 0xb0, 0, 0, NULL, 0, 128);
    g_assert_cmphex(end.status, ==, 0x6282);
    g_assert_cmpuint(end.size, <, 128);
}
static void read_only(void)
{
    CardSession s = { .selected = TRUE };
    guint8 data[] = { 0x01, 0x02 };
    select_file(&s, 4);
    g_assert_cmphex(card_process(&s, 0, 0xd6, 0, 0, data, sizeof(data), 0).status, ==, 0x6982);
    g_assert_cmphex(card_process(&s, 0, 0xff, 0, 0, NULL, 0, 0).status, ==, 0x6d00);
    g_assert_cmphex(card_process(&s, 0x80, 0xb0, 0, 0, NULL, 0, 1).status, ==, 0x6e00);
    g_assert_cmphex(select_file(&s, 5).status, ==, 0x6a82);
}
static void sessions(void)
{
    CardSession a = { .selected = TRUE }, b = { .selected = TRUE };
    select_file(&a, 4);
    g_assert_cmphex(card_process(&b, 0, 0xb0, 0, 0, NULL, 0, 2).status, ==, 0x6986);
    card_session_reset(&a);
    g_assert_cmphex(card_process(&a, 0, 0xb0, 0, 0, NULL, 0, 2).status, ==, 0x6985);
}
int main(int argc, char** argv)
{
    g_test_init(&argc, &argv, NULL);
    g_test_add_func("/card/read-text", text_read);
    g_test_add_func("/card/bounds", bounds);
    g_test_add_func("/card/read-only", read_only);
    g_test_add_func("/card/session-isolation", sessions);
    return g_test_run();
}
