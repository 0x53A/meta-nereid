"""Bounded client/listener supervision with listener-first cancellation.

Author: Lukas Rieger <code@lukasrieger.com>
"""
import signal
import subprocess
import time


class CleanupIncomplete(RuntimeError):
    def __init__(self, live_pids, errors):
        self.live_pids = tuple(live_pids)
        self.errors = tuple(errors)
        super().__init__(
            f"cleanup incomplete; live_pids={self.live_pids}; errors={self.errors}"
        )


def cleanup_processes(listener, client, client_finished, timeout=10):
    """Signal the listener before terminating a client blocked in QSEE."""
    if timeout < 0:
        raise ValueError("negative cleanup timeout")
    deadline = time.monotonic() + timeout
    cancelled = not client_finished
    if client is not None and client.poll() is None:
        cancelled = True
    errors = []

    def send(proc, signo, label):
        if proc is None or proc.poll() is not None:
            return
        try:
            proc.send_signal(signo)
        except ProcessLookupError:
            pass
        except OSError as exc:
            errors.append(f"{label}: {exc}")

    # Unregister first to release the QSEE client's listener wait.
    send(listener, signal.SIGINT if cancelled else signal.SIGTERM,
         "listener signal")
    if cancelled:
        send(client, signal.SIGKILL, "client signal")

    statuses = {}
    for label, proc in (("listener", listener), ("client", client)):
        if proc is None:
            statuses[label] = None
            continue
        try:
            statuses[label] = proc.wait(
                timeout=max(0, deadline - time.monotonic())
            )
        except subprocess.TimeoutExpired:
            statuses[label] = None
        except OSError as exc:
            errors.append(f"{label} wait: {exc}")
            statuses[label] = None
    live_pids = []
    for label, proc in (("listener", listener), ("client", client)):
        if proc is None:
            continue
        status = proc.poll()
        if status is None:
            live_pids.append(proc.pid)
        else:
            statuses[label] = status
    if live_pids or errors:
        raise CleanupIncomplete(live_pids, errors)
    return statuses
