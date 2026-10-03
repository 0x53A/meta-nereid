/* Bounded PIN-change and per-user-clear command/state helpers.
 * Author: Lukas Rieger <code@lukasrieger.com>
 */
#ifndef NEREID_GATEKEEPER_MANAGEMENT_H
#define NEREID_GATEKEEPER_MANAGEMENT_H

#include <stddef.h>
#include <stdint.h>
#include <string.h>

#include "auth-ipc.h"

#define GK_MANAGEMENT_CHANGE_COMMAND UINT32_C(0x1001)
#define GK_MANAGEMENT_DELETE_COMMAND UINT32_C(0x1003)
#define GK_MANAGEMENT_AUTH_REJECTED_STATUS INT32_C(-30)
#define GK_MANAGEMENT_CHANGE_HEADER 32U
#define GK_MANAGEMENT_DELETE_HEADER 8U

struct credential {
    unsigned char bytes[AUTH_IPC_MAX_HANDLE];
    uint32_t length;
};

struct gk_management_ops {
    int (*verify)(void *opaque, uint32_t uid,
                  const unsigned char *handle, uint32_t handle_length,
                  const unsigned char *pin, uint16_t pin_length,
                  int32_t *status, uint64_t *sid);
    int (*change)(void *opaque, const struct auth_ipc_request *input,
                  int32_t *status, struct credential *new_handle);
    int (*delete_user)(void *opaque, uint32_t uid, int32_t *status);
};

struct gk_management_result {
    uint32_t status;
    struct credential handle;
};

static void gk_management_put_u32le(unsigned char *p, uint32_t value)
{
    p[0] = (unsigned char)value;
    p[1] = (unsigned char)(value >> 8);
    p[2] = (unsigned char)(value >> 16);
    p[3] = (unsigned char)(value >> 24);
}

static int gk_management_pin_valid(const unsigned char *pin, size_t length)
{
    return auth_ipc_management_pin_valid(pin, length);
}

static int gk_management_build_change(
    unsigned char *wire, size_t capacity, uint32_t uid,
    const unsigned char *handle, uint32_t handle_length,
    const unsigned char *current_pin, uint16_t current_pin_length,
    const unsigned char *new_pin, uint32_t new_pin_length,
    size_t *request_length)
{
    if (!wire || !request_length || !uid || !handle || !handle_length ||
        handle_length > AUTH_IPC_MAX_HANDLE ||
        !gk_management_pin_valid(current_pin, current_pin_length) ||
        !gk_management_pin_valid(new_pin, new_pin_length))
        return -1;
    size_t length = GK_MANAGEMENT_CHANGE_HEADER + (size_t)handle_length +
                    current_pin_length + new_pin_length;
    if (length > capacity)
        return -1;
    memset(wire, 0, length);
    gk_management_put_u32le(wire, GK_MANAGEMENT_CHANGE_COMMAND);
    gk_management_put_u32le(wire + 4, uid);
    gk_management_put_u32le(wire + 8, GK_MANAGEMENT_CHANGE_HEADER);
    gk_management_put_u32le(wire + 12, handle_length);
    size_t pin_offset = GK_MANAGEMENT_CHANGE_HEADER + (size_t)handle_length;
    gk_management_put_u32le(wire + 16, (uint32_t)pin_offset);
    gk_management_put_u32le(wire + 20, current_pin_length);
    size_t new_pin_offset = pin_offset + current_pin_length;
    gk_management_put_u32le(wire + 24, (uint32_t)new_pin_offset);
    gk_management_put_u32le(wire + 28, new_pin_length);
    memcpy(wire + GK_MANAGEMENT_CHANGE_HEADER, handle, handle_length);
    memcpy(wire + pin_offset, current_pin, current_pin_length);
    memcpy(wire + new_pin_offset, new_pin, new_pin_length);
    *request_length = length;
    return 0;
}

static int gk_management_build_delete(unsigned char wire[GK_MANAGEMENT_DELETE_HEADER],
                                      uint32_t uid)
{
    if (!wire || !uid)
        return -1;
    memset(wire, 0, GK_MANAGEMENT_DELETE_HEADER);
    gk_management_put_u32le(wire, GK_MANAGEMENT_DELETE_COMMAND);
    gk_management_put_u32le(wire + 4, uid);
    return 0;
}

/* Return 1 only for the known generic verification rejection before mutation.
 * This code does not establish that a PIN was wrong. All other Gatekeeper
 * statuses and every post-mutation failure are indeterminate and fatal.
 */
static int gk_management_run(const struct auth_ipc_request *input,
                             const struct gk_management_ops *ops,
                             void *opaque,
                             struct gk_management_result *result)
{
    int rc = -1;
    int32_t status = INT32_MIN;
    uint64_t verified_sid = 0, changed_sid = 0;
    struct credential changed_handle = {0};
    if (!input || !ops || !ops->verify || !result || !input->uid ||
        !input->handle_length || input->handle_length > AUTH_IPC_MAX_HANDLE ||
        !gk_management_pin_valid(input->pin, input->pin_length))
        goto out;
    memset(result, 0, sizeof(*result));
    if (input->operation == AUTH_IPC_CHANGE) {
        if (!ops->change ||
            !gk_management_pin_valid(input->new_pin, input->new_pin_length))
            goto out;
    } else if (input->operation == AUTH_IPC_CLEAR) {
        if (!ops->delete_user || input->new_pin_length != 0)
            goto out;
    } else {
        goto out;
    }

    if (ops->verify(opaque, input->uid, input->handle, input->handle_length,
                    input->pin, input->pin_length, &status, &verified_sid))
        goto out;
    if (status == GK_MANAGEMENT_AUTH_REJECTED_STATUS) {
        result->status = 1;
        rc = 1;
        goto out;
    }
    if (status != 0)
        goto out;
    /* A successful Gatekeeper SID must identify the same user in Keymaster. */
    if (verified_sid == 0)
        goto out;

    if (input->operation == AUTH_IPC_CLEAR) {
        status = INT32_MIN;
        if (ops->delete_user(opaque, input->uid, &status) || status != 0)
            goto out;
        result->status = 0;
        rc = 0;
        goto out;
    }

    status = INT32_MIN;
    if (ops->change(opaque, input, &status, &changed_handle) || status != 0 ||
        !changed_handle.length || changed_handle.length > AUTH_IPC_MAX_HANDLE)
        goto out;
    status = INT32_MIN;
    if (ops->verify(opaque, input->uid, changed_handle.bytes,
                    changed_handle.length, input->new_pin,
                    (uint16_t)input->new_pin_length, &status, &changed_sid) ||
        status != 0 || changed_sid != verified_sid)
        goto out;
    result->status = 0;
    memcpy(&result->handle, &changed_handle, sizeof(result->handle));
    rc = 0;
out:
    if (rc < 0 && result)
        explicit_bzero(result, sizeof(*result));
    explicit_bzero(&changed_handle, sizeof(changed_handle));
    explicit_bzero(&verified_sid, sizeof(verified_sid));
    explicit_bzero(&changed_sid, sizeof(changed_sid));
    explicit_bzero(&status, sizeof(status));
    return rc;
}

#endif
