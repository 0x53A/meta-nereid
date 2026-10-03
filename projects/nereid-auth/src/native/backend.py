#!/usr/bin/python3
"""One bounded Gatekeeper request with a temporary authenticated RPMB listener.

The Rust service owns UID generation and durable handle storage. This program
accepts one NGK1/NGK2/NGK3 frame on stdin and returns one NGR1/NGR2/NGR3 frame on stdout. It never
logs request bytes, PINs, handles, or authentication tokens.

Author: Lukas Rieger <code@lukasrieger.com>
"""
import fcntl
import os
import pathlib
import stat
import subprocess
import sys
import threading
import time

from supervisor import cleanup_processes


ROOT = pathlib.Path(__file__).resolve().parent
CLIENT = ROOT / "nereid-gatekeeper-backend"
LISTENER = ROOT / "rpmb-listener"
LOCK_PATH = pathlib.Path("/run/nereid-auth-backend.lock")


def acquire_backend_lock(path=LOCK_PATH, expected_uid=0):
    fd = os.open(
        path,
        os.O_RDWR | os.O_CREAT | os.O_CLOEXEC | os.O_NOFOLLOW,
        0o600,
    )
    try:
        item = os.fstat(fd)
        if (not stat.S_ISREG(item.st_mode) or item.st_uid != expected_uid
                or stat.S_IMODE(item.st_mode) != 0o600):
            raise RuntimeError("unexpected backend lock file")
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        return fd
    except Exception:
        os.close(fd)
        raise RuntimeError("backend service is already active or lock is unsafe")


def ensure_qsee_unowned():
    try:
        result = subprocess.run(
            ["fuser", "/dev/qseecom"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            timeout=3,
        )
    except OSError as exc:
        raise RuntimeError("QSEE ownership checker unavailable") from exc
    if result.returncode != 1 or result.stdout or result.stderr:
        raise RuntimeError("QSEE device already has an owner")


def start_listener():
    if not CLIENT.is_file() or not LISTENER.is_file():
        raise RuntimeError("backend executable set is incomplete")
    log_pipe = subprocess.Popen(
        [str(LISTENER)],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        close_fds=True,
        bufsize=0,
    )
    announced_ready = threading.Event()
    log_failure = []

    def drain_listener_log():
        try:
            assert log_pipe.stdout is not None
            for line in iter(log_pipe.stdout.readline, b""):
                # The reviewed listener logs only operation metadata and never
                # RPMB frames, MACs, handles, PINs, or tokens.
                sys.stderr.buffer.write(line)
                sys.stderr.buffer.flush()
                if line.startswith(b"READY "):
                    announced_ready.set()
        except (OSError, BrokenPipeError) as exc:
            log_failure.append(str(exc))

    reader = threading.Thread(target=drain_listener_log, daemon=True)
    reader.start()
    deadline = time.monotonic() + 10
    try:
        while not announced_ready.wait(0.1):
            if log_pipe.poll() is not None:
                raise RuntimeError("RPMB listener exited before ready")
            if time.monotonic() >= deadline:
                raise RuntimeError("RPMB listener readiness timeout")
        if log_pipe.poll() is not None:
            raise RuntimeError("RPMB listener exited during startup")
        if log_failure:
            raise RuntimeError("RPMB listener log transport failed")
    except Exception:
        cleanup_processes(log_pipe, None, client_finished=True, timeout=10)
        reader.join(timeout=1)
        raise
    return log_pipe, reader


def main():
    if os.geteuid() != 0:
        raise RuntimeError("backend service requires root")
    lock_fd = acquire_backend_lock()
    listener = None
    client = None
    reader = None
    client_finished = False
    cleanup_status = None
    try:
        ensure_qsee_unowned()
        listener, reader = start_listener()
        client = subprocess.Popen(
            [str(CLIENT)],
            # Relay the parent's bounded IPC pipe directly to C. Python never
            # reads or copies a PIN or opaque credential into its own memory.
            stdin=None,
            stdout=None,
            stderr=None,
            close_fds=True,
        )
        try:
            client_status = client.wait(timeout=50)
            client_finished = True
        except subprocess.TimeoutExpired as exc:
            raise RuntimeError("Gatekeeper helper timeout; request not retried") from exc
        if client_status != 0:
            raise RuntimeError(f"Gatekeeper helper failed with exit {client_status}")

        listener_to_clean, client_to_clean = listener, client
        listener = None
        client = None
        cleanup_status = cleanup_processes(
            listener_to_clean, client_to_clean,
            client_finished=True, timeout=10,
        )
        if cleanup_status.get("listener") != 0:
            raise RuntimeError("RPMB listener did not stop cleanly")

        # The C helper emits one bounded NGR1/NGR2/NGR3 frame after a complete TEE
        # response. Its exit status and listener cleanup status gate service
        # success; the Rust parent validates the returned frame and status.
        sys.stderr.write("backend_completed=1\n")
        sys.stderr.flush()
        return 0
    finally:
        if listener is not None or client is not None:
            # cleanup_processes emits listener cancellation before SIGKILLing a
            # potentially blocked QSEE client and shares one bounded deadline.
            cleanup_processes(
                listener, client,
                client_finished=client_finished,
                timeout=10,
            )
        if reader is not None:
            reader.join(timeout=1)
        os.close(lock_fd)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:
        # Do not include request contents or PIN-bearing values in diagnostics.
        sys.stderr.write(f"backend_error={type(exc).__name__}: {exc}\n")
        raise SystemExit(1)
