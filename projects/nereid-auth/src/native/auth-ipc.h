/* Bounded NGK1/NGK2/NGK3 stdin request parser shared with the native backend.
 * Author: Lukas Rieger <code@lukasrieger.com>
 */
#ifndef NEREID_AUTH_IPC_H
#define NEREID_AUTH_IPC_H

#include <errno.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <unistd.h>

#define AUTH_IPC_REQUEST_HEADER 20U
#define AUTH_IPC_MAX_HANDLE 1024U
#define AUTH_IPC_MAX_PIN 64U
#define AUTH_IPC_MANAGEMENT_MAX_PIN 12U
#define AUTH_IPC_MAX_WRAPPED (76U + 4096U)
#define AUTH_IPC_MAX_REQUEST \
    (AUTH_IPC_REQUEST_HEADER + AUTH_IPC_MAX_HANDLE + AUTH_IPC_MAX_PIN + AUTH_IPC_MAX_WRAPPED)

enum auth_ipc_operation {
    AUTH_IPC_ENROLL = 1, AUTH_IPC_VERIFY = 2,
    AUTH_IPC_WRAP = 3, AUTH_IPC_UNWRAP = 4,
    AUTH_IPC_CHANGE = 5, AUTH_IPC_CLEAR = 6
};

struct auth_ipc_request {
    uint32_t operation;
    uint32_t uid;
    uint16_t handle_length;
    uint16_t pin_length;
    uint32_t wrapped_length;
    uint32_t new_pin_length;
    unsigned char handle[AUTH_IPC_MAX_HANDLE];
    unsigned char pin[AUTH_IPC_MAX_PIN];
    unsigned char new_pin[AUTH_IPC_MANAGEMENT_MAX_PIN];
    unsigned char wrapped[AUTH_IPC_MAX_WRAPPED];
};

static uint32_t auth_ipc_get_u32le(const unsigned char *p)
{
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) |
           ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

static int auth_ipc_read_exact(int fd, unsigned char *buffer, size_t length)
{
    size_t done = 0;
    while (done < length) {
        ssize_t count = read(fd, buffer + done, length - done);
        if (count < 0 && errno == EINTR)
            continue;
        if (count <= 0)
            return -1;
        done += (size_t)count;
    }
    return 0;
}

static int auth_ipc_management_pin_valid(const unsigned char *pin, size_t length)
{
    if (!pin || length < 4U || length > AUTH_IPC_MANAGEMENT_MAX_PIN)
        return 0;
    for (size_t i = 0; i < length; ++i)
        if (pin[i] < '0' || pin[i] > '9')
            return 0;
    return 1;
}

static inline int auth_ipc_read_request(int fd, struct auth_ipc_request *out)
{
    unsigned char wire[AUTH_IPC_MAX_REQUEST] = {0};
    unsigned char trailing = 0;
    int rc = -1;
    if (!out)
        goto done;
    memset(out, 0, sizeof(*out));
    if (auth_ipc_read_exact(fd, wire, AUTH_IPC_REQUEST_HEADER))
        goto done;
    int keymaster = memcmp(wire, "NGK2", 4) == 0;
    int management = memcmp(wire, "NGK3", 4) == 0;
    if (!keymaster && !management && memcmp(wire, "NGK1", 4))
        goto done;

    out->operation = auth_ipc_get_u32le(wire + 4);
    out->uid = auth_ipc_get_u32le(wire + 8);
    out->handle_length =
        (uint16_t)(wire[12] | ((uint16_t)wire[13] << 8));
    out->pin_length =
        (uint16_t)(wire[14] | ((uint16_t)wire[15] << 8));
    if (management)
        out->new_pin_length = auth_ipc_get_u32le(wire + 16);
    else
        out->wrapped_length = auth_ipc_get_u32le(wire + 16);

    if (management) {
        if (!out->uid || !out->handle_length ||
            out->handle_length > AUTH_IPC_MAX_HANDLE ||
            out->pin_length < 4U ||
            out->pin_length > AUTH_IPC_MANAGEMENT_MAX_PIN ||
            out->new_pin_length > AUTH_IPC_MANAGEMENT_MAX_PIN ||
            (out->operation == AUTH_IPC_CHANGE &&
             out->new_pin_length < 4U))
            goto done;
        /* Validate the payload PINs after reading their bounded bytes below. */
        if ((out->operation == AUTH_IPC_CLEAR && out->new_pin_length != 0U) ||
            (out->operation != AUTH_IPC_CHANGE &&
             out->operation != AUTH_IPC_CLEAR))
            goto done;
    } else if (!out->uid || !out->pin_length ||
               out->pin_length > AUTH_IPC_MAX_PIN ||
               out->handle_length > AUTH_IPC_MAX_HANDLE ||
               out->wrapped_length > AUTH_IPC_MAX_WRAPPED) {
        goto done;
    }

    if (management) {
        /* NGK3 has its own exact operation and PIN rules above. */
    } else if (!keymaster) {
        if (out->wrapped_length ||
            (out->operation == AUTH_IPC_ENROLL && out->handle_length != 0) ||
            (out->operation == AUTH_IPC_VERIFY && out->handle_length == 0) ||
            (out->operation != AUTH_IPC_ENROLL && out->operation != AUTH_IPC_VERIFY))
            goto done;
    } else {
        if (!out->handle_length ||
            (out->operation == AUTH_IPC_WRAP && out->wrapped_length != 0) ||
            (out->operation == AUTH_IPC_UNWRAP && out->wrapped_length < 77U) ||
            (out->operation != AUTH_IPC_WRAP && out->operation != AUTH_IPC_UNWRAP))
            goto done;
    }

    size_t payload_length = (size_t)out->handle_length + out->pin_length +
                            (management ? out->new_pin_length : out->wrapped_length);
    if (auth_ipc_read_exact(fd, wire + AUTH_IPC_REQUEST_HEADER,
                            payload_length))
        goto done;
    for (;;) {
        ssize_t count = read(fd, &trailing, sizeof(trailing));
        if (count < 0 && errno == EINTR)
            continue;
        if (count != 0)
            goto done;
        break;
    }

    memcpy(out->handle, wire + AUTH_IPC_REQUEST_HEADER, out->handle_length);
    memcpy(out->pin,
           wire + AUTH_IPC_REQUEST_HEADER + out->handle_length,
           out->pin_length);
    if (management) {
        memcpy(out->new_pin,
               wire + AUTH_IPC_REQUEST_HEADER + out->handle_length + out->pin_length,
               out->new_pin_length);
        if (!auth_ipc_management_pin_valid(out->pin, out->pin_length) ||
            (out->operation == AUTH_IPC_CHANGE &&
             !auth_ipc_management_pin_valid(out->new_pin, out->new_pin_length)))
            goto done;
    } else {
        memcpy(out->wrapped,
               wire + AUTH_IPC_REQUEST_HEADER + out->handle_length + out->pin_length,
               out->wrapped_length);
    }
    rc = 0;
done:
    explicit_bzero(wire, sizeof(wire));
    explicit_bzero(&trailing, sizeof(trailing));
    if (rc && out)
        explicit_bzero(out, sizeof(*out));
    return rc;
}

#endif
