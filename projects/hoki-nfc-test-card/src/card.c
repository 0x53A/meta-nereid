/* SPDX-License-Identifier: BSD-3-Clause */
#include "card.h"
#include <string.h>

/* NFC Forum Type 4 NDEF application, never a payment application. */
const guint8 card_aid[7] = { 0xd2, 0x76, 0x00, 0x00, 0x85, 0x01, 0x01 };
/* NLEN followed by a short well-known Text record, language "en". */
static const guint8 ndef[] = {
    0x00, 7 + sizeof(TEST_CARD_TEXT) - 1,
    0xd1, 0x01, 3 + sizeof(TEST_CARD_TEXT) - 1, 'T', 0x02, 'e', 'n',
    'H', 'o', 'k', 'i', ' ', 'N', 'F', 'C', ' ', 't', 'e', 's', 't', ' ', 'c', 'a', 'r', 'd'
};
/* CC length 15, mapping 2.0, MLe 128, MLc 52, NDEF E104, read-only. */
static const guint8 cc[] = {
    0x00, 0x0f, 0x20, 0x00, 0x80, 0x00, 0x34,
    0x04, 0x06, 0xe1, 0x04, 0x00, sizeof(ndef), 0x00, 0xff
};
G_STATIC_ASSERT(sizeof(ndef) == 2 + 7 + sizeof(TEST_CARD_TEXT) - 1);

void card_session_reset(CardSession* session)
{
    *session = (CardSession) { 0 };
}

CardReply card_process(CardSession* session, guint8 cla, guint8 ins,
    guint8 p1, guint8 p2, const guint8* data, gsize size, guint le)
{
    CardReply reply = { .status = 0x9000 };
    if (!session->selected) reply.status = 0x6985;
    else if (cla) reply.status = 0x6e00;
    else if (ins == 0xa4) {
        if (p1 != 0 || p2 != 0x0c) reply.status = 0x6a86;
        else if (size != 2 || le) reply.status = 0x6700;
        else if (data[0] == 0xe1 && data[1] == 0x03) session->file = CARD_FILE_CC;
        else if (data[0] == 0xe1 && data[1] == 0x04) session->file = CARD_FILE_NDEF;
        else reply.status = 0x6a82;
    } else if (ins == 0xb0) {
        guint offset = ((guint)p1 << 8) | p2;
        if (p1 & 0x80) reply.status = 0x6a86;
        else if (size || !le) reply.status = 0x6700;
        else if (le > 128) reply.status = 0x6c80;
        else if (session->file == CARD_FILE_NONE) reply.status = 0x6986;
        else {
            const guint8* file = session->file == CARD_FILE_CC ? cc : ndef;
            gsize length = session->file == CARD_FILE_CC ? sizeof(cc) : sizeof(ndef);
            if (offset >= length) reply.status = 0x6b00;
            else {
                reply.size = MIN(le, length - offset);
                reply.data = file + offset;
                if (le > length - offset) reply.status = 0x6282;
            }
        }
    } else if (ins == 0xd6) reply.status = 0x6982; /* Write denied. */
    else reply.status = 0x6d00;
    return reply;
}
