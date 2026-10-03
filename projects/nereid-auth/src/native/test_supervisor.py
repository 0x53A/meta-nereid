"""Host-only checks for QSEE preflight and listener-first cleanup.

Author: Lukas Rieger <code@lukasrieger.com>
"""
import fcntl
import os
import signal
import stat
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from backend import acquire_backend_lock, ensure_qsee_unowned
from supervisor import cleanup_processes


EVENTS = []


class FakeProcess:
    next_pid = 100

    def __init__(self, name, wait_status=0, completed=False):
        self.name = name
        self.wait_status = wait_status
        self.returncode = wait_status if completed else None
        self.sent = None
        self.pid = FakeProcess.next_pid
        FakeProcess.next_pid += 1

    def poll(self):
        return self.returncode

    def send_signal(self, signo):
        EVENTS.append(("signal", self.name, signo))
        self.sent = signo

    def wait(self, timeout=None):
        EVENTS.append(("wait", self.name))
        self.returncode = self.wait_status
        return self.returncode


class BackendSupervisorTests(unittest.TestCase):
    def setUp(self):
        EVENTS.clear()

    def test_normal_cleanup_stops_listener_after_client(self):
        listener = FakeProcess("listener")
        client = FakeProcess("client", completed=True)
        statuses = cleanup_processes(listener, client, client_finished=True)
        self.assertEqual(statuses, {"listener": 0, "client": 0})
        self.assertEqual(EVENTS[0], ("signal", "listener", signal.SIGTERM))
        self.assertEqual(EVENTS[1:], [("wait", "listener"), ("wait", "client")])

    def test_cancel_signals_listener_before_client_and_waits(self):
        listener = FakeProcess("listener")
        client = FakeProcess("client", wait_status=-signal.SIGKILL)
        statuses = cleanup_processes(listener, client, client_finished=False)
        self.assertEqual(statuses, {"listener": 0, "client": -signal.SIGKILL})
        self.assertEqual(
            EVENTS,
            [
                ("signal", "listener", signal.SIGINT),
                ("signal", "client", signal.SIGKILL),
                ("wait", "listener"),
                ("wait", "client"),
            ],
        )

    def test_backend_lock_is_exclusive_and_private(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "backend.lock"
            first = acquire_backend_lock(path, expected_uid=os.geteuid())
            try:
                mode = stat.S_IMODE(os.fstat(first).st_mode)
                self.assertEqual(mode, 0o600)
                second = os.open(path, os.O_RDWR | os.O_CLOEXEC)
                try:
                    with self.assertRaises(BlockingIOError):
                        fcntl.flock(second, fcntl.LOCK_EX | fcntl.LOCK_NB)
                finally:
                    os.close(second)
            finally:
                os.close(first)

    def test_unsafe_lock_permissions_fail_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "backend.lock"
            path.touch(mode=0o600)
            path.chmod(0o644)
            with self.assertRaises(RuntimeError):
                acquire_backend_lock(path, expected_uid=os.geteuid())

    def test_qsee_preflight_requires_no_owners_and_silent_fuser(self):
        no_owner = SimpleNamespace(returncode=1, stdout=b"", stderr=b"")
        with patch("backend.subprocess.run", return_value=no_owner) as run:
            ensure_qsee_unowned()
        run.assert_called_once()
        self.assertEqual(run.call_args.args[0], ["fuser", "/dev/qseecom"])

        for result in (
            SimpleNamespace(returncode=0, stdout=b"123\n", stderr=b""),
            SimpleNamespace(returncode=1, stdout=b"", stderr=b"diagnostic"),
            SimpleNamespace(returncode=2, stdout=b"", stderr=b""),
        ):
            with self.subTest(result=result):
                with patch("backend.subprocess.run", return_value=result):
                    with self.assertRaises(RuntimeError):
                        ensure_qsee_unowned()


if __name__ == "__main__":
    unittest.main()
