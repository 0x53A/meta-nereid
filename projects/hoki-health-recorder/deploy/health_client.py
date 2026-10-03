"""Bounded, connection-scoped API to the health collection daemon."""
import json
import socket

class HealthClient:
    def __init__(self, path='/run/hoki-health-policy/control.sock'):
        self.socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.socket.settimeout(5)
        self.socket.connect(path)
        self.reader = self.socket.makefile('rb')

    def request(self, command='status', **fields):
        self.socket.sendall((json.dumps(dict(command=command, **fields)) + '\n').encode())
        line = self.reader.readline(65537)
        if len(line) > 65536 or not line.endswith(b'\n'):
            raise RuntimeError('Invalid health daemon response')
        reply = json.loads(line)
        if reply.get('ok') is not True:
            raise RuntimeError(reply.get('error', 'Health request failed'))
        return reply

    def close(self):
        self.reader.close()
        self.socket.close()
