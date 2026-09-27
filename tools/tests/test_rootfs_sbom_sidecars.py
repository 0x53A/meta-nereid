import hashlib
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[2] / "recipes-core/hoki-rootfs/files/hoki-rootfs.py"
spec = importlib.util.spec_from_file_location("hoki_rootfs_sbom_test", SCRIPT)
manager = importlib.util.module_from_spec(spec)
spec.loader.exec_module(manager)


class SbomSidecarTest(unittest.TestCase):
    def test_stage_verifies_sidecar_before_publication(self):
        with tempfile.TemporaryDirectory() as tmp:
            store = Path(tmp)
            incoming = store / "incoming" / "v1"
            incoming.mkdir(parents=True)
            (store / "versions").mkdir()
            files = {
                "rootfs.ext4": b"fake rootfs",
                "recovery.img": b"ANDROID!fake recovery",
                "sbom.spdx.json": b'{"@graph":[]}',
                "licenses.tsv": b"package\tapp\t1\tMIT\n",
            }
            data = {"format": 1, "version": "v1"}
            for filename, body in files.items():
                (incoming / filename).write_bytes(body)
                key = {"rootfs.ext4": "rootfs", "recovery.img": "recovery"}.get(filename, filename)
                data[key + "_sha256"] = hashlib.sha256(body).hexdigest()
                data[key + "_size"] = len(body)
            (incoming / "manifest.json").write_text(json.dumps(data))
            (incoming / "sbom.spdx.json").write_text("tampered")
            with self.assertRaisesRegex(ValueError, "sbom.spdx.json"):
                manager.stage(store, incoming)
            self.assertFalse((store / "versions" / "v1").exists())
            (incoming / "sbom.spdx.json").write_bytes(files["sbom.spdx.json"])
            with patch.object(manager.subprocess, "run"):
                self.assertEqual(manager.stage(store, incoming), "v1")
            self.assertEqual((store / "versions" / "v1" / "sbom.spdx.json").read_bytes(),
                             files["sbom.spdx.json"])


if __name__ == "__main__":
    unittest.main()
