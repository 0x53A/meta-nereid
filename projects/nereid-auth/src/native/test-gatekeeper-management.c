/* Host-only Gatekeeper management wire/flow fixtures. No QSEE access.
 * Author: Lukas Rieger <code@lukasrieger.com>
 */
#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include "gatekeeper-management.h"

static const unsigned char old_handle[] = {0x21, 0x22, 0x23};
static const unsigned char changed_handle[] = {0x31, 0x32, 0x33, 0x34};
static const unsigned char current_pin[] = "1234";
static const unsigned char new_pin[] = "567890";
static const uint64_t expected_sid = UINT64_C(0x0102030405060708);

struct fake {
    char calls[8];
    unsigned call_count;
    unsigned verify_count;
    unsigned change_count;
    unsigned delete_count;
    int verify_transport_error;
    int change_transport_error;
    int delete_transport_error;
    int32_t verify_status[2];
    uint64_t verify_sid[2];
    int32_t change_status;
    int32_t delete_status;
};

static uint32_t u32le(const unsigned char *p)
{
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8) |
           ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

static void record_call(struct fake *fake, char call)
{
    assert(fake->call_count < sizeof(fake->calls));
    fake->calls[fake->call_count++] = call;
}

static int fake_verify(void *opaque, uint32_t uid,
                       const unsigned char *handle, uint32_t handle_length,
                       const unsigned char *pin, uint16_t pin_length,
                       int32_t *status, uint64_t *sid)
{
    struct fake *fake = opaque;
    assert(uid == UINT32_C(0xf1234567));
    unsigned index = fake->verify_count++;
    assert(index < 2);
    if (index == 0) {
        record_call(fake, 'V');
        assert(handle_length == sizeof(old_handle));
        assert(memcmp(handle, old_handle, sizeof(old_handle)) == 0);
        assert(pin_length == sizeof(current_pin) - 1);
        assert(memcmp(pin, current_pin, sizeof(current_pin) - 1) == 0);
    } else {
        record_call(fake, 'v');
        assert(handle_length == sizeof(changed_handle));
        assert(memcmp(handle, changed_handle, sizeof(changed_handle)) == 0);
        assert(pin_length == sizeof(new_pin) - 1);
        assert(memcmp(pin, new_pin, sizeof(new_pin) - 1) == 0);
    }
    if (fake->verify_transport_error == (int)index + 1)
        return -1;
    *status = fake->verify_status[index];
    *sid = fake->verify_sid[index];
    return 0;
}

static int fake_change(void *opaque, const struct auth_ipc_request *input,
                       int32_t *status, struct credential *handle)
{
    struct fake *fake = opaque;
    unsigned char wire[GK_MANAGEMENT_CHANGE_HEADER +
                       AUTH_IPC_MAX_HANDLE + 24U] = {0};
    size_t length = 0;
    fake->change_count++;
    record_call(fake, 'C');
    assert(input->operation == AUTH_IPC_CHANGE);
    assert(gk_management_build_change(wire, sizeof(wire), input->uid,
               input->handle, input->handle_length, input->pin,
               input->pin_length, input->new_pin, input->new_pin_length,
               &length) == 0);
    assert(u32le(wire) == GK_MANAGEMENT_CHANGE_COMMAND);
    assert(u32le(wire + 4) == input->uid);
    assert(u32le(wire + 8) == GK_MANAGEMENT_CHANGE_HEADER);
    assert(u32le(wire + 12) == sizeof(old_handle));
    assert(u32le(wire + 16) == GK_MANAGEMENT_CHANGE_HEADER + sizeof(old_handle));
    assert(u32le(wire + 20) == sizeof(current_pin) - 1);
    assert(u32le(wire + 24) == GK_MANAGEMENT_CHANGE_HEADER +
                               sizeof(old_handle) + sizeof(current_pin) - 1);
    assert(u32le(wire + 28) == sizeof(new_pin) - 1);
    assert(length == GK_MANAGEMENT_CHANGE_HEADER + sizeof(old_handle) +
                     sizeof(current_pin) - 1 + sizeof(new_pin) - 1);
    assert(memcmp(wire + GK_MANAGEMENT_CHANGE_HEADER,
                  old_handle, sizeof(old_handle)) == 0);
    assert(memcmp(wire + u32le(wire + 16),
                  current_pin, sizeof(current_pin) - 1) == 0);
    assert(memcmp(wire + u32le(wire + 24),
                  new_pin, sizeof(new_pin) - 1) == 0);
    explicit_bzero(wire, sizeof(wire));
    if (fake->change_transport_error)
        return -1;
    *status = fake->change_status;
    if (*status == 0) {
        memcpy(handle->bytes, changed_handle, sizeof(changed_handle));
        handle->length = sizeof(changed_handle);
    }
    return 0;
}

static int fake_delete(void *opaque, uint32_t uid, int32_t *status)
{
    struct fake *fake = opaque;
    unsigned char wire[GK_MANAGEMENT_DELETE_HEADER] = {0};
    fake->delete_count++;
    record_call(fake, 'D');
    assert(gk_management_build_delete(wire, uid) == 0);
    assert(u32le(wire) == GK_MANAGEMENT_DELETE_COMMAND);
    assert(u32le(wire + 4) == UINT32_C(0xf1234567));
    explicit_bzero(wire, sizeof(wire));
    if (fake->delete_transport_error)
        return -1;
    *status = fake->delete_status;
    return 0;
}

static const struct gk_management_ops ops = {
    .verify = fake_verify,
    .change = fake_change,
    .delete_user = fake_delete,
};

static void prepare(struct auth_ipc_request *input, uint32_t operation)
{
    memset(input, 0, sizeof(*input));
    input->operation = operation;
    input->uid = UINT32_C(0xf1234567);
    input->handle_length = sizeof(old_handle);
    input->pin_length = sizeof(current_pin) - 1;
    memcpy(input->handle, old_handle, sizeof(old_handle));
    memcpy(input->pin, current_pin, sizeof(current_pin) - 1);
    if (operation == AUTH_IPC_CHANGE) {
        input->new_pin_length = sizeof(new_pin) - 1;
        memcpy(input->new_pin, new_pin, sizeof(new_pin) - 1);
    }
}

static struct fake good_change(void)
{
    struct fake fake = {0};
    fake.verify_sid[0] = expected_sid;
    fake.verify_sid[1] = expected_sid;
    return fake;
}

static int all_zero(const void *memory, size_t length)
{
    const unsigned char *bytes = memory;
    for (size_t i = 0; i < length; ++i)
        if (bytes[i] != 0)
            return 0;
    return 1;
}

int main(void)
{
    struct auth_ipc_request input;
    struct gk_management_result result;
    unsigned checks = 0;

    prepare(&input, AUTH_IPC_CHANGE);
    struct fake fake = good_change();
    memset(&result, 0xa5, sizeof(result));
    assert(gk_management_run(&input, &ops, &fake, &result) == 0);
    assert(fake.call_count == 3 && memcmp(fake.calls, "VCv", 3) == 0);
    assert(fake.verify_count == 2 && fake.change_count == 1 && fake.delete_count == 0);
    assert(result.status == 0 && result.handle.length == sizeof(changed_handle));
    assert(memcmp(result.handle.bytes, changed_handle, sizeof(changed_handle)) == 0);
    explicit_bzero(&fake, sizeof(fake));
    explicit_bzero(&result, sizeof(result));
    ++checks;

    fake = good_change();
    fake.verify_sid[1] = 0; /* SID equality also rejects a zero new SID. */
    assert(gk_management_run(&input, &ops, &fake, &result) == -1);
    assert(fake.call_count == 3 && memcmp(fake.calls, "VCv", 3) == 0);
    assert(all_zero(&result, sizeof(result)));
    explicit_bzero(&fake, sizeof(fake));
    ++checks;

    fake = good_change();
    fake.verify_sid[0] = 0;
    assert(gk_management_run(&input, &ops, &fake, &result) == -1);
    assert(fake.call_count == 1 && fake.calls[0] == 'V');
    assert(fake.change_count == 0 && fake.delete_count == 0);
    assert(all_zero(&result, sizeof(result)));
    explicit_bzero(&fake, sizeof(fake));
    ++checks;

    fake = good_change();
    fake.verify_status[1] = GK_MANAGEMENT_AUTH_REJECTED_STATUS;
    assert(gk_management_run(&input, &ops, &fake, &result) == -1);
    assert(fake.call_count == 3 && memcmp(fake.calls, "VCv", 3) == 0);
    assert(all_zero(&result, sizeof(result)));
    explicit_bzero(&fake, sizeof(fake));
    ++checks;

    fake = good_change();
    fake.verify_status[0] = GK_MANAGEMENT_AUTH_REJECTED_STATUS;
    assert(gk_management_run(&input, &ops, &fake, &result) == 1);
    assert(fake.call_count == 1 && fake.calls[0] == 'V');
    assert(fake.change_count == 0 && result.status == 1 && result.handle.length == 0);
    explicit_bzero(&fake, sizeof(fake));
    explicit_bzero(&result, sizeof(result));
    ++checks;

    fake = good_change();
    fake.verify_status[0] = -24;
    assert(gk_management_run(&input, &ops, &fake, &result) == -1);
    assert(fake.call_count == 1 && fake.change_count == 0);
    assert(all_zero(&result, sizeof(result)));
    explicit_bzero(&fake, sizeof(fake));
    ++checks;

    fake = good_change();
    fake.change_status = GK_MANAGEMENT_AUTH_REJECTED_STATUS;
    assert(gk_management_run(&input, &ops, &fake, &result) == -1);
    assert(fake.call_count == 2 && memcmp(fake.calls, "VC", 2) == 0);
    assert(all_zero(&result, sizeof(result)));
    explicit_bzero(&fake, sizeof(fake));
    ++checks;

    prepare(&input, AUTH_IPC_CLEAR);
    memset(&result, 0xa5, sizeof(result));
    fake = good_change();
    assert(gk_management_run(&input, &ops, &fake, &result) == 0);
    assert(fake.call_count == 2 && memcmp(fake.calls, "VD", 2) == 0);
    assert(fake.verify_count == 1 && fake.delete_count == 1);
    assert(result.status == 0 && result.handle.length == 0);
    explicit_bzero(&fake, sizeof(fake));
    explicit_bzero(&result, sizeof(result));
    ++checks;

    fake = good_change();
    fake.verify_sid[0] = 0;
    assert(gk_management_run(&input, &ops, &fake, &result) == -1);
    assert(fake.call_count == 1 && fake.calls[0] == 'V');
    assert(fake.delete_count == 0);
    assert(all_zero(&result, sizeof(result)));
    explicit_bzero(&fake, sizeof(fake));
    ++checks;

    fake = good_change();
    fake.verify_status[0] = GK_MANAGEMENT_AUTH_REJECTED_STATUS;
    assert(gk_management_run(&input, &ops, &fake, &result) == 1);
    assert(fake.call_count == 1 && fake.delete_count == 0 && result.status == 1);
    explicit_bzero(&fake, sizeof(fake));
    explicit_bzero(&result, sizeof(result));
    ++checks;

    fake = good_change();
    fake.delete_status = -1;
    assert(gk_management_run(&input, &ops, &fake, &result) == -1);
    assert(fake.call_count == 2 && fake.delete_count == 1);
    assert(all_zero(&result, sizeof(result)));
    explicit_bzero(&fake, sizeof(fake));
    ++checks;

    unsigned char wire[GK_MANAGEMENT_CHANGE_HEADER + AUTH_IPC_MAX_HANDLE + 24U] = {0};
    size_t request_length = 0;
    assert(gk_management_build_change(wire, sizeof(wire), input.uid,
               input.handle, input.handle_length, current_pin,
               sizeof(current_pin) - 1, new_pin, sizeof(new_pin) - 1,
               &request_length) == 0);
    assert(request_length == GK_MANAGEMENT_CHANGE_HEADER + sizeof(old_handle) +
                             sizeof(current_pin) - 1 + sizeof(new_pin) - 1);
    assert(gk_management_build_change(wire, sizeof(wire), input.uid,
               input.handle, input.handle_length,
               (const unsigned char *)"12x4", 4, new_pin,
               sizeof(new_pin) - 1, &request_length) != 0);
    assert(gk_management_build_change(wire, sizeof(wire), input.uid,
               input.handle, input.handle_length, current_pin,
               sizeof(current_pin) - 1, (const unsigned char *)"123", 3,
               &request_length) != 0);
    unsigned char delete_wire[GK_MANAGEMENT_DELETE_HEADER] = {0};
    assert(gk_management_build_delete(delete_wire, input.uid) == 0);
    assert(gk_management_build_delete(delete_wire, 0) != 0);
    explicit_bzero(wire, sizeof(wire));
    explicit_bzero(delete_wire, sizeof(delete_wire));
    explicit_bzero(&input, sizeof(input));
    printf("Gatekeeper management fixtures passed (%u flow cases, command layouts, PIN bounds)\n", checks);
    return 0;
}
