/* Host-only checks for the exact NGK1 parser used by the ARM helper.
 * Author: Lukas Rieger <code@lukasrieger.com>
 */
#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#include "auth-ipc.h"

#define TEST_FRAME_MAX AUTH_IPC_MAX_REQUEST

static void put_u16le(unsigned char *p, uint16_t value)
{
    p[0] = (unsigned char)value;
    p[1] = (unsigned char)(value >> 8);
}

static void put_u32le(unsigned char *p, uint32_t value)
{
    p[0] = (unsigned char)value;
    p[1] = (unsigned char)(value >> 8);
    p[2] = (unsigned char)(value >> 16);
    p[3] = (unsigned char)(value >> 24);
}

static int parse_bytes(const unsigned char *frame, size_t length,
                       struct auth_ipc_request *request)
{
    int fds[2];
    assert(pipe(fds) == 0);
    size_t written = 0;
    while (written < length) {
        ssize_t count = write(fds[1], frame + written, length - written);
        assert(count > 0);
        written += (size_t)count;
    }
    assert(close(fds[1]) == 0);
    int rc = auth_ipc_read_request(fds[0], request);
    assert(close(fds[0]) == 0);
    return rc;
}

static void header(unsigned char *frame, uint32_t operation, uint32_t uid,
                   uint16_t handle_length, uint16_t pin_length)
{
    memset(frame, 0, TEST_FRAME_MAX);
    memcpy(frame, "NGK1", 4);
    put_u32le(frame + 4, operation);
    put_u32le(frame + 8, uid);
    put_u16le(frame + 12, handle_length);
    put_u16le(frame + 14, pin_length);
}

static void management_header(unsigned char *frame, uint32_t operation,
                              uint32_t uid, uint16_t handle_length,
                              uint16_t current_pin_length,
                              uint32_t new_pin_length)
{
    header(frame, operation, uid, handle_length, current_pin_length);
    memcpy(frame, "NGK3", 4);
    put_u32le(frame + 16, new_pin_length);
}

int main(void)
{
    unsigned char frame[TEST_FRAME_MAX] = {0};
    struct auth_ipc_request request;
    unsigned checks = 0;

    header(frame, AUTH_IPC_DEVICE_WRAP, 0, 0, 0);
    memcpy(frame,"NGD1",4);
    assert(parse_bytes(frame,20,&request)==0);
    assert(request.uid==0 && request.handle_length==0 && request.pin_length==0);
    put_u32le(frame+8,7); assert(parse_bytes(frame,20,&request)!=0);
    put_u32le(frame+8,0); put_u16le(frame+14,4); assert(parse_bytes(frame,24,&request)!=0);
    put_u16le(frame+14,0); put_u32le(frame+4,AUTH_IPC_UNWRAP); assert(parse_bytes(frame,20,&request)!=0);
    put_u32le(frame+4,AUTH_IPC_DEVICE_UNWRAP); assert(parse_bytes(frame,20,&request)!=0);
    put_u32le(frame+16,77); assert(parse_bytes(frame,97,&request)==0);
    assert(parse_bytes(frame,96,&request)!=0);
    assert(parse_bytes(frame,98,&request)!=0);
    memcpy(frame,"NGK2",4); assert(parse_bytes(frame,97,&request)!=0);
    checks+=9;

    header(frame, AUTH_IPC_ENROLL, 0xf1234567U, 0, 4);
    memcpy(frame + AUTH_IPC_REQUEST_HEADER, "test", 4);
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER + 4, &request) == 0);
    assert(request.operation == AUTH_IPC_ENROLL);
    assert(request.uid == 0xf1234567U);
    assert(request.handle_length == 0 && request.pin_length == 4);
    assert(memcmp(request.pin, "test", 4) == 0);
    explicit_bzero(&request, sizeof(request));
    ++checks;

    header(frame, AUTH_IPC_VERIFY, 0x87654321U, 3, 2);
    memcpy(frame + AUTH_IPC_REQUEST_HEADER, "hdlpin", 6);
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER + 5, &request) == 0);
    assert(request.operation == AUTH_IPC_VERIFY);
    assert(request.uid == 0x87654321U);
    assert(request.handle_length == 3 && request.pin_length == 2);
    assert(memcmp(request.handle, "hdl", 3) == 0);
    assert(memcmp(request.pin, "pi", 2) == 0);
    explicit_bzero(&request, sizeof(request));
    ++checks;
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER + 6, &request) != 0);
    ++checks;

    header(frame, AUTH_IPC_ENROLL, 7, 0, 1);
    frame[0] = 'X';
    frame[AUTH_IPC_REQUEST_HEADER] = 'x';
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER + 1, &request) != 0);
    ++checks;
    header(frame, 3, 7, 0, 1);
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER + 1, &request) != 0);
    ++checks;
    header(frame, AUTH_IPC_ENROLL, 0, 0, 1);
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER + 1, &request) != 0);
    ++checks;
    header(frame, AUTH_IPC_ENROLL, 7, 0, 1);
    put_u32le(frame + 16, 1);
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER + 1, &request) != 0);
    ++checks;
    header(frame, AUTH_IPC_ENROLL, 7, 0, 0);
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER, &request) != 0);
    ++checks;
    header(frame, AUTH_IPC_ENROLL, 7, 0, AUTH_IPC_MAX_PIN + 1);
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER, &request) != 0);
    ++checks;
    header(frame, AUTH_IPC_ENROLL, 7, 1, 1);
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER + 2, &request) != 0);
    ++checks;
    header(frame, AUTH_IPC_VERIFY, 7, 0, 1);
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER + 1, &request) != 0);
    ++checks;
    header(frame, AUTH_IPC_VERIFY, 7, 1, 1);
    frame[AUTH_IPC_REQUEST_HEADER] = 'h';
    frame[AUTH_IPC_REQUEST_HEADER + 1] = 'p';
    frame[AUTH_IPC_REQUEST_HEADER + 2] = 'x';
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER + 3, &request) != 0);
    ++checks;

    header(frame, AUTH_IPC_WRAP, 7, 3, 4);
    memcpy(frame, "NGK2", 4);
    memcpy(frame + AUTH_IPC_REQUEST_HEADER, "hdl1234", 7);
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER + 7, &request) == 0);
    assert(request.operation == AUTH_IPC_WRAP && request.wrapped_length == 0);
    ++checks;
    put_u32le(frame + 4, AUTH_IPC_UNWRAP);
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER + 7, &request) != 0);
    ++checks;
    put_u32le(frame + 16, 77);
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER + 7 + 77, &request) == 0);
    assert(request.wrapped_length == 77);
    ++checks;
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER + 7 + 76, &request) != 0);
    ++checks;
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER + 7 + 78, &request) != 0);
    ++checks;
    put_u32le(frame + 16, AUTH_IPC_MAX_WRAPPED + 1);
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER, &request) != 0);
    ++checks;
    put_u32le(frame + 4, AUTH_IPC_VERIFY);
    put_u32le(frame + 16, 0);
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER + 7, &request) != 0);
    ++checks;

    management_header(frame, AUTH_IPC_CHANGE, 0xf1234567U, 3, 4, 6);
    memcpy(frame + AUTH_IPC_REQUEST_HEADER, "hdl1234567890", 13);
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER + 13, &request) == 0);
    assert(request.operation == AUTH_IPC_CHANGE);
    assert(request.uid == 0xf1234567U);
    assert(request.handle_length == 3 && request.pin_length == 4);
    assert(request.new_pin_length == 6);
    assert(memcmp(request.handle, "hdl", 3) == 0);
    assert(memcmp(request.pin, "1234", 4) == 0);
    assert(memcmp(request.new_pin, "567890", 6) == 0);
    explicit_bzero(&request, sizeof(request));
    ++checks;
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER + 14, &request) != 0);
    ++checks;

    management_header(frame, AUTH_IPC_CLEAR, 7, 1, 4, 0);
    memcpy(frame + AUTH_IPC_REQUEST_HEADER, "h1234", 5);
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER + 5, &request) == 0);
    assert(request.operation == AUTH_IPC_CLEAR && request.new_pin_length == 0);
    explicit_bzero(&request, sizeof(request));
    ++checks;
    management_header(frame, AUTH_IPC_CLEAR, 7, 1, 4, 4);
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER, &request) != 0);
    ++checks;
    management_header(frame, AUTH_IPC_CHANGE, 7, 1, 3, 4);
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER, &request) != 0);
    ++checks;
    management_header(frame, AUTH_IPC_CHANGE, 7, 1, 4, 3);
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER, &request) != 0);
    ++checks;
    management_header(frame, AUTH_IPC_CHANGE, 7, 1, 4, 13);
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER, &request) != 0);
    ++checks;
    management_header(frame, AUTH_IPC_CHANGE, 7, 1, 4, 4);
    memcpy(frame + AUTH_IPC_REQUEST_HEADER, "h12a45678", 9);
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER + 9, &request) != 0);
    ++checks;
    management_header(frame, AUTH_IPC_CHANGE, 7, 0, 4, 4);
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER, &request) != 0);
    ++checks;
    management_header(frame, AUTH_IPC_CHANGE, 0, 1, 4, 4);
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER, &request) != 0);
    ++checks;
    management_header(frame, 7, 7, 1, 4, 0);
    assert(parse_bytes(frame, AUTH_IPC_REQUEST_HEADER, &request) != 0);
    ++checks;

    explicit_bzero(frame, sizeof(frame));
    printf("auth IPC parser checks passed: %u\n", checks);
    return 0;
}
