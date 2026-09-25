/* SPDX-License-Identifier: BSD-3-Clause */
#ifndef HOKI_LINUX_TARGET_H
#define HOKI_LINUX_TARGET_H
#include <nfc_target.h>
NfcTarget* linux_target_new(int fd, guint protocol, GObject* owner);
void linux_target_close(NfcTarget* target);
#endif
