import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "sbom-license-index.py"
spec = importlib.util.spec_from_file_location("sbom_license_index", SCRIPT)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class LicenseIndexTest(unittest.TestCase):
    def test_installed_packages_and_present_cargo_artifacts_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            graph = [
                {"type": "software_Package", "spdxId": "pkg-a", "name": "alpha",
                 "software_primaryPurpose": "install", "software_packageVersion": "1.0"},
                {"type": "software_Package", "spdxId": "pkg-unused", "name": "unused",
                 "software_primaryPurpose": "install", "software_packageVersion": "2.0"},
                {"type": "simplelicensing_LicenseExpression", "spdxId": "lic-mit",
                 "simplelicensing_licenseExpression": "MIT"},
                {"type": "Relationship", "from": "pkg-a", "to": ["lic-mit"],
                 "relationshipType": "hasDeclaredLicense"},
                {"type": "software_File", "spdxId": "file-app", "name": "usr/lib/watch-app"},
                {"type": "Relationship", "from": "pkg-a", "to": ["file-app"],
                 "relationshipType": "contains"},
            ]
            (root / "image.spdx.json").write_text(json.dumps({"@graph": graph}))
            (root / "image.manifest").write_text("alpha arm 1.0-r0\n")
            cargo = root / "cargo" / "watch"; cargo.mkdir(parents=True)
            for artifact in ("watch-app", "uninstalled-app"):
                (cargo / f"{artifact}.licenses.json").write_text(json.dumps({
                    "artifact": artifact,
                    "crates": [
                        {"name": "shared", "version": "1.0", "licenseDeclared": "Apache-2.0"},
                        {"name": "shared", "version": "1.1", "licenseDeclared": "MIT"},
                    ],
                }))
            rows = module.index(root / "image.spdx.json", root / "image.manifest", root / "cargo")
            self.assertEqual(rows, [
                ("package", "alpha", "1.0", "MIT"),
                ("crate", "shared", "1.0", "Apache-2.0"),
                ("crate", "shared", "1.1", "MIT"),
            ])


if __name__ == "__main__":
    unittest.main()
