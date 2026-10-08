"""Storage safety tests; no real crash or Qualcomm hardware needed."""
import importlib.machinery
import importlib.util
from pathlib import Path
import tempfile
import io
import struct
import unittest
from unittest.mock import patch
import fcntl
import os

loader = importlib.machinery.SourceFileLoader("collector", str(Path(__file__).parent / "files/hoki-crashdump"))
spec = importlib.util.spec_from_loader(loader.name, loader)
c = importlib.util.module_from_spec(spec)
loader.exec_module(c)


class StorageTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.scope = patch.object(c, "ROOT", self.root)
        self.scope.start()
        self.addCleanup(self.scope.stop)

    def capture(self, size, cap=1024):
        with tempfile.TemporaryFile() as f:
            f.write(b"x" * size)
            f.seek(0)
            return c.capture(f.fileno(), "test", cap)

    def test_complete_and_truncated(self):
        self.assertTrue(self.capture(100)["complete"])
        result = self.capture(2000)
        self.assertEqual(result["bytes"], 1024)
        self.assertEqual(result["reason"], "size-limit")
        self.assertFalse(result["complete"])
        for p in self.root.glob("dump-*"):
            self.assertEqual(p.stat().st_mode & 0o777, 0o600)

    def test_count_retention(self):
        for _ in range(7):
            self.capture(100)
        self.assertEqual(len(list(self.root.glob("*.bin"))), 4)
        self.assertEqual(len(list(self.root.glob("*.json"))), 4)

    def test_budget_includes_metadata(self):
        with patch.object(c, "TOTAL", 10000):
            for _ in range(4):
                self.capture(1000)
        self.assertLessEqual(sum(p.stat().st_size for p in self.root.glob("dump-*")), 10000)
        self.assertEqual(len(list(self.root.glob("*.bin"))), 1)

    def test_elf_truncation(self):
        header = bytearray(52)
        header[:6] = b"\x7fELF\x01\x01"
        struct.pack_into("<I", header, 28, 52)
        struct.pack_into("<HH", header, 42, 32, 1)
        phdr = struct.pack("<IIIIIIII", 1, 84, 0, 0, 10, 10, 0, 0)
        path = self.root / "core"
        path.write_bytes(header + phdr + b"x" * 10)
        self.assertTrue(c.elf_complete(path))
        path.write_bytes(header + phdr + b"x" * 5)
        self.assertFalse(c.elf_complete(path))
        with path.open("rb") as source:
            result = c.capture(source.fileno(), "test", 1024,
                               {"elf_complete": c.elf_complete(path)})
        self.assertFalse(result["complete"])
        self.assertEqual(result["reason"], "incomplete-elf")

    def test_orphan_metadata_pruned(self):
        (self.root / "dump-orphan.json").write_text("{}")
        self.capture(100)
        self.assertFalse((self.root / "dump-orphan.json").exists())

    def test_inotify_copies_closed_core(self):
        incoming = self.root / "incoming"
        incoming.mkdir()
        with patch.object(c, "BUFFER", incoming):
            fd = c.watch_cores()
            try:
                path = incoming / "core.123.0.0.6.1234"
                path.write_bytes(b"test truncated ELF")
                c.core_events(fd)
                self.assertFalse(path.exists())
                self.assertEqual(len(list(self.root.glob("*.bin"))), 1)
            finally:
                os.close(fd)

    def test_low_space(self):
        with patch.object(c, "free_bytes", return_value=c.RESERVE):
            self.assertIsNone(self.capture(100))
        self.assertFalse(list(self.root.glob("dump-*")))

    def test_short_writes_are_retried(self):
        class ShortWriter(io.BytesIO):
            def write(self, data):
                return super().write(data[:2])
        output = ShortWriter()
        c.write_all(output, b"complete payload")
        self.assertEqual(output.getvalue(), b"complete payload")

    def test_failed_archive_preserves_original(self):
        path = self.root / "core.123.0.0.6.1234"
        path.write_bytes(b"original core")
        for reason in ("io-error:disk full", "timeout", "free-space-limit"):
            with patch.object(c, "capture", return_value={"bytes": 2, "reason": reason}):
                c.collect_core(path)
            self.assertEqual(path.read_bytes(), b"original core")

    def test_concurrent_capture_skipped(self):
        with (self.root / ".lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            self.assertIsNone(self.capture(100))

    def test_interrupted_capture_is_pruned(self):
        for n in range(5):
            (self.root / ("dump-%s-test.bin" % n)).write_bytes(b"partial")
        self.capture(100)
        self.assertEqual(len(list(self.root.glob("*.bin"))), 4)

    def test_space_runs_out_mid_capture(self):
        with patch.object(c, "free_bytes", side_effect=[c.RESERVE + 100000, c.RESERVE]):
            self.assertEqual(self.capture(100)["reason"], "free-space-limit")


if __name__ == "__main__":
    unittest.main()
