/* SPDX-License-Identifier: BSD-3-Clause */
#ifndef HOKI_TEST_CARD_H
#define HOKI_TEST_CARD_H
#include <glib.h>

#define TEST_CARD_TEXT "Hoki NFC test card"
typedef enum { CARD_FILE_NONE, CARD_FILE_CC, CARD_FILE_NDEF } CardFile;
typedef struct { gboolean selected; CardFile file; } CardSession;
typedef struct { const guint8* data; gsize size; guint16 status; } CardReply;

extern const guint8 card_aid[7];
void card_session_reset(CardSession* session);
CardReply card_process(CardSession* session, guint8 cla, guint8 ins,
    guint8 p1, guint8 p2, const guint8* data, gsize size, guint le);
#endif
