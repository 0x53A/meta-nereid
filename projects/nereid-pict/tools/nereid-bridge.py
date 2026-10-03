#!/usr/bin/env python3
"""Development SSH endpoint. Normal Nereid Wayland capture + desktop uinput.

Run only through authenticated SSH, never as a network listener. Pict's host
owns pairing/grants. Separate capture/input SSH processes avoid shared payload
queues. Input devices use USB bus identity, routed exclusively to Nereid desktop.
"""
import array
import fcntl
import json
import os
import socket
import struct
import sys
import time


def pack(*values):
    return struct.pack('<' + 'I' * len(values), *values)


def string(value):
    value = value.encode() + b'\0'
    return pack(len(value)) + value + b'\0' * (-len(value) % 4)


def read_string(data, offset=0):
    length = struct.unpack_from('<I', data, offset)[0]
    return data[offset+4:offset+3+length].decode(), offset+4+(length+3)//4*4


class Wayland:
    def __init__(self):
        self.socket = socket.socket(socket.AF_UNIX)
        self.socket.settimeout(10)
        self.socket.connect('/run/user/1000/wayland-0')
        self.next_id = 2
        self.kinds = {1: 'display'}
        self.globals = {}
        self.heads = {}
        self.outputs = {}
        self.serial = 0
        self.buffer = b''
        self.results = {}
        self.dimensions = {}
        self.stopped = set()
        self.frames = {}
        self.registry = self.new('registry')
        self.send(1, 1, pack(self.registry))
        self.sync()

    def new(self, kind):
        value = self.next_id
        self.next_id += 1
        self.kinds[value] = kind
        return value

    def send(self, obj, opcode, payload=b'', fd=None):
        data = pack(obj, ((len(payload)+8)<<16)|opcode) + payload
        if fd is None:
            self.socket.sendall(data)
        else:
            assert self.socket.sendmsg([data], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array('i', [fd]))]) == len(data)

    def event(self):
        def receive():
            data = self.socket.recv(65536)
            if not data:
                raise RuntimeError('Wayland disconnected')
            self.buffer += data
        while len(self.buffer) < 8:
            receive()
        obj, header = struct.unpack_from('<II', self.buffer)
        size, opcode = header >> 16, header & 65535
        while len(self.buffer) < size:
            receive()
        data, self.buffer = self.buffer[8:size], self.buffer[size:]
        kind = self.kinds.get(obj)
        if kind == 'display' and opcode == 0:
            raise RuntimeError('Wayland protocol error: ' + repr(data))
        if kind == 'display' and opcode == 1:
            self.kinds.pop(struct.unpack('<I', data)[0], None)
        if kind == 'registry' and opcode == 0:
            name = struct.unpack_from('<I', data)[0]
            interface, end = read_string(data, 4)
            self.globals[name] = (interface, struct.unpack_from('<I', data, end)[0])
        if kind == 'registry' and opcode == 1:
            self.globals.pop(struct.unpack('<I', data)[0], None)
        if kind == 'manager' and opcode == 0:
            child = struct.unpack('<I', data)[0]
            self.kinds[child] = 'head'
            self.heads[child] = {}
        if kind == 'manager' and opcode == 1:
            self.serial = struct.unpack('<I', data)[0]
        if kind == 'head' and opcode == 0:
            self.heads[obj]['name'] = read_string(data)[0]
        if kind == 'head' and opcode == 4:
            self.heads[obj]['enabled'] = struct.unpack('<I', data)[0]
        if kind == 'output' and opcode == 4:
            self.outputs[obj] = read_string(data)[0]
        if kind == 'config':
            self.results[obj] = opcode
        if kind == 'session' and opcode == 0:
            self.dimensions[obj] = struct.unpack('<II', data)
        if kind == 'session' and opcode == 5:
            self.stopped.add(obj)
        if kind == 'frame' and opcode in (3, 4):
            self.frames[obj] = opcode == 3
        return obj, opcode

    def sync(self):
        callback = self.new('callback')
        self.send(1, 0, pack(callback))
        while self.event() != (callback, 0):
            pass

    def bind(self, interface, version=1, name=None, kind=None):
        if name is None:
            name = next(n for n, (v, _) in self.globals.items() if v == interface)
        obj = self.new(kind or interface)
        self.send(self.registry, 0, pack(name) + string(interface) + pack(version, obj))
        return obj

    def configure(self, enabled, width, height, scale, refresh=15000):
        manager = self.bind('zwlr_output_manager_v1', kind='manager')
        self.sync()
        tx = self.new('config')
        self.send(manager, 0, pack(tx, self.serial))
        for head, spec in self.heads.items():
            if spec['name'] == 'hoki-display':
                self.send(tx, 0, pack(self.new('config-head'), head))
            elif spec['name'] == 'hoki-desktop':
                if enabled:
                    hc = self.new('config-head')
                    self.send(tx, 0, pack(hc, head))
                    self.send(hc, 1, pack(width, height, refresh))
                    self.send(hc, 4, pack(scale * 256 // 100))
                else:
                    self.send(tx, 1, pack(head))
        self.send(tx, 2)
        self.sync()
        assert self.results[tx] == 0, 'Output configuration rejected/stale'
        self.send(tx, 4)
        self.sync()

    def capture(self):
        for name, (interface, version) in list(self.globals.items()):
            if interface == 'wl_output':
                self.bind(interface, 4, name, 'output')
        sm = self.bind('ext_output_image_capture_source_manager_v1')
        cm = self.bind('ext_image_copy_capture_manager_v1')
        shm = self.bind('wl_shm')
        self.sync()
        output = next(i for i, name in self.outputs.items() if name == 'hoki-desktop')
        session = None
        fd = None
        buffer = None
        try:
            while sys.stdin.buffer.read(1) == b'f':
                while True:
                    self.sync()
                    if session is None or session in self.stopped:
                        if session is not None:
                            self.send(session, 1)
                            self.stopped.discard(session)
                            self.dimensions.pop(session, None)
                            self.send(buffer, 0)
                            os.close(fd)
                            fd = None
                        source = self.new('source')
                        self.send(sm, 0, pack(source, output))
                        session = self.new('session')
                        self.send(cm, 0, pack(session, source, 1))  # paint cursor
                        self.send(source, 0)
                        self.sync()
                        if session in self.stopped:
                            raise RuntimeError('Desktop output disabled')
                        width, height = self.dimensions[session]
                        assert 64 <= width <= 1920 and 64 <= height <= 1920
                        size = width * height * 4
                        fd = os.memfd_create('pict-desktop')
                        os.ftruncate(fd, size)
                        pool = self.new('pool')
                        self.send(shm, 0, pack(pool, size), fd)
                        buffer = self.new('buffer')
                        self.send(pool, 0, pack(buffer, 0, width, height, width*4, 1))
                        self.send(pool, 1)
                    frame = self.new('frame')
                    self.send(session, 0, pack(frame))
                    self.send(frame, 1, pack(buffer))
                    self.send(frame, 3)
                    while frame not in self.frames:
                        self.event()
                    success = self.frames.pop(frame)
                    self.send(frame, 0)
                    if success:
                        pixels = os.pread(fd, size, 0)
                        assert len(pixels) == size
                        sys.stdout.buffer.write(pack(width, height))
                        sys.stdout.buffer.write(pixels)
                        sys.stdout.buffer.flush()
                        break
        finally:
            if fd is not None:
                os.close(fd)
            self.socket.close()


class Device:
    def __init__(self, name, pointer=False):
        self.fd = os.open('/dev/uinput', os.O_WRONLY | os.O_NONBLOCK)
        self.held = set()
        self.pointer = pointer
        self.wheel = [0.0, 0.0]
        fcntl.ioctl(self.fd, 0x40045564, 1)  # EV_KEY
        for code in (range(272, 277) if pointer else range(1, 256)):
            fcntl.ioctl(self.fd, 0x40045565, code)
        maxima = [0] * 64
        if pointer:
            fcntl.ioctl(self.fd, 0x40045564, 3)  # EV_ABS
            for axis in (0, 1):
                fcntl.ioctl(self.fd, 0x40045567, axis)
                maxima[axis] = 65535
            fcntl.ioctl(self.fd, 0x40045564, 2)  # EV_REL
            for axis in (6, 8):
                fcntl.ioctl(self.fd, 0x40045566, axis)
            fcntl.ioctl(self.fd, 0x4004556e, 0)  # INPUT_PROP_POINTER
        # BUS_USB makes classification unambiguous; Nereid routes this bus to desktop.
        desc = struct.pack('80sHHHHi', name.encode(), 3, 1, 1, 1, 0)
        desc += struct.pack('256i', *(maxima + [0] * 192))
        os.write(self.fd, desc)
        fcntl.ioctl(self.fd, 0x5501)

    def emit(self, events):
        events.append((0, 0, 0))
        data = b''.join(struct.pack('llHHi', 0, 0, *e) for e in events)
        assert os.write(self.fd, data) == len(data)

    def key(self, code, down):
        if (code in self.held) == down:
            return
        if down:
            self.held.add(code)
        else:
            self.held.discard(code)
        self.emit([(1, code, int(down))])

    def release(self):
        for code in list(self.held):
            self.key(code, False)

    def close(self):
        self.release()
        fcntl.ioctl(self.fd, 0x5502)
        os.close(self.fd)


def input_loop():
    keyboard = Device('Pict Nereid desktop keyboard')
    pointer = None
    try:
        pointer = Device('Pict Nereid desktop mouse', True)
        time.sleep(.3)  # allow udev/libinput discovery before accepting input
        sys.stdout.buffer.write(b'OK')
        sys.stdout.buffer.flush()
        for line in sys.stdin:
            if len(line) > 4096:
                raise RuntimeError('Oversized input command')
            data = json.loads(line)
            kind = data['type']
            if kind == 'release':
                keyboard.release()
                pointer.release()
            elif kind == 'key':
                code = int(data['code'])
                assert 1 <= code < 256
                keyboard.key(code, bool(data['pressed']))
            elif kind in ('pointer', 'wheel'):
                event = data if kind == 'pointer' else data['event']
                x, y = float(event['x']), float(event['y'])
                assert 0 <= x <= 1 and 0 <= y <= 1
                pointer.emit([(3, 0, round(x*65535)), (3, 1, round(y*65535))])
                if kind == 'pointer':
                    for mask, code in [(1,272),(2,273),(4,274),(8,275),(16,276)]:
                        pointer.key(code, bool(int(event['buttons']) & mask))
                else:
                    unit = 1 if event['delta_mode'] == 1 else (20 if event['delta_mode'] == 2 else 1/15)
                    for i, axis, delta, sign in [(0,6,event['delta_x'],1),(1,8,event['delta_y'],-1)]:
                        pointer.wheel[i] += max(-100,min(100,float(delta)*unit))*sign
                        ticks = int(pointer.wheel[i])
                        pointer.wheel[i] -= ticks
                        if ticks:
                            pointer.emit([(2,axis,ticks)])
    finally:
        if pointer is not None:
            pointer.close()
        keyboard.close()


if __name__ == '__main__':
    mode = sys.argv[1]
    if mode == 'input':
        input_loop()
    elif mode == 'configure':
        Wayland().configure(*map(int, sys.argv[2:]))
    elif mode == 'venus':
        os.execv('/userdata/pict-demo/nereid-venus', ['nereid-venus'])
    elif mode == 'capture':
        Wayland().capture()
    else:
        raise RuntimeError('Unknown bridge command')
