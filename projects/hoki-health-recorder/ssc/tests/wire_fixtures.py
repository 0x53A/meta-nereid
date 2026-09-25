import struct

def varint(n):
    out = bytearray()
    while n > 127:
        out.append((n & 127) | 128)
        n >>= 7
    return bytes(out + bytes([n]))


def chunk(data=b'', tx=1, eof=1, eop=1):
    return b'\x0a' + varint(len(data)) + data + b'\x10' + varint(tx) + bytes([24, eof, 32, eop])


def file_bytes():
    b = bytearray(24)
    struct.pack_into('<HI', b, 2, 0x80, len(b))
    return bytes(b)


def indication(source, payload):
    event = b'\x0d' + struct.pack('<I', 1028) + b'\x11' + struct.pack('<Q', 123) + b'\x1a' + varint(len(payload)) + payload
    body = b'\x0a\x12\x09' + source[:8] + b'\x11' + source[8:] + b'\x12' + varint(len(event)) + event
    tlv = b'\x02' + struct.pack('<HH', len(body) + 2, len(body)) + body
    return 'IND 33 ' + tlv.hex()
