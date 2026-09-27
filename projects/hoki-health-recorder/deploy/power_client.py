"""Connection-owned power requests; no subprocess or shell command protocol."""
import json
import socket
import time


class PowerClient:
    def __init__(self):
        self.socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.socket.settimeout(5)
        self.socket.connect('/run/hoki-powerd/control.sock')
        self.reader = self.socket.makefile('rb')

    def request(self, command, **fields):
        self.socket.sendall((json.dumps(dict(version=1, command=command, **fields)) + '\n').encode())
        line = self.reader.readline(16385)
        if len(line) > 16384 or not line.endswith(b'\n'):
            raise RuntimeError('Invalid power coordinator response')
        reply = json.loads(line)
        if reply.get('ok') is not True:
            raise RuntimeError(reply.get('error', 'Power coordinator failed'))
        return reply

    def inhibit(self, reason):
        deadline = time.monotonic() + 10
        while True:
            try:
                self.request('inhibit', cpu=True, display=False, reason=reason)
                return
            except RuntimeError as error:
                if 'sleep transition in progress' not in str(error) or time.monotonic() >= deadline:
                    raise
                time.sleep(0.1)

    def close(self):
        self.reader.close()
        self.socket.close()
