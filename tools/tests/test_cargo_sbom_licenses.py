import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "cargo-sbom-licenses.py"
spec = importlib.util.spec_from_file_location("cargo_sbom_licenses", SCRIPT)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class CargoSbomLicensesTest(unittest.TestCase):
    def test_only_compiled_crates_get_licenses(self):
        with tempfile.TemporaryDirectory() as tmp:
            precursor = Path(tmp) / "app.cargo-sbom.json"
            precursor.write_text(json.dumps({
                "version": 1,
                "root": 0,
                "crates": [
                    {"id": "app", "kind": ["bin"], "features": [],
                     "dependencies": [{"index": 1, "kind": "normal"}]},
                    {"id": "used", "kind": ["lib"], "features": ["std"],
                     "dependencies": []},
                ],
            }))
            packages = {
                "app": {"name": "app", "version": "1", "source": None,
                        "license": None, "license_file": None, "repository": None},
                "used": {"name": "used", "version": "2", "source": "registry+crates.io",
                         "license": "MIT OR Apache-2.0", "license_file": None,
                         "repository": "https://example.org/used"},
                "unused": {"name": "unused", "version": "3", "source": "registry+crates.io",
                           "license": "GPL-3.0-only", "license_file": None,
                           "repository": None},
            }
            report = module.license_report(precursor, packages)
            self.assertEqual([crate["name"] for crate in report["crates"]], ["app", "used"])
            self.assertEqual(report["crates"][1]["licenseDeclared"], "MIT OR Apache-2.0")
            self.assertEqual(report["needsReview"], [0])


if __name__ == "__main__":
    unittest.main()
