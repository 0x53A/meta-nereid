import unittest
from pathlib import Path
import subprocess
import sys
import tempfile
from metadata_json import decode_metadata


class MetadataJson(unittest.TestCase):
    def test_nonfinite_numbers_are_rejected_in_nested_metadata(self):
        for value in ('NaN', 'Infinity', '-Infinity', '1e400', '-1e400'):
            with self.subTest(value=value), self.assertRaisesRegex(ValueError, 'finite'):
                decode_metadata('{"selected":[{"sensor":{"extra":' + value + '}}]}')
        self.assertEqual(decode_metadata('{"numbers":[1,-2,1.25,1e300,-1e-300]}'),
                         {'numbers': [1, -2, 1.25, 1e300, -1e-300]})
        self.assertEqual(decode_metadata('{"name":"NaN"}'), {'name': 'NaN'})

    def test_coverage_cli_rejects_nonfinite_controller_metadata(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'controller.json').write_text('{"version":1,"extra":{"value":NaN}}')
            result = subprocess.run([sys.executable, str(Path(__file__).with_name('hal_coverage.py')),
                                     str(root)], capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(result.stdout, '')
            self.assertIn('non-finite metadata number', result.stderr)

    def test_coverage_cli_rejects_duplicate_controller_metadata(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'controller.json').write_text('{"version":1,"version":1}')
            result = subprocess.run([sys.executable, str(Path(__file__).with_name('hal_coverage.py')),
                                     str(root)], capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('duplicate metadata field', result.stderr)

    def test_duplicate_nested_sensor_fields_are_rejected(self):
        with self.assertRaisesRegex(ValueError, 'duplicate metadata field'):
            decode_metadata('{"selected":[{"sensor":{"handle":1,"handle":2}}]}')
        self.assertEqual(decode_metadata('{"a":{"handle":1},"b":{"handle":2}}'),
                         {'a': {'handle': 1}, 'b': {'handle': 2}})

    def test_root_must_be_an_object(self):
        for text in ('[]', 'null', 'true', '1', '"metadata"'):
            with self.subTest(text=text), self.assertRaisesRegex(ValueError, 'JSON object'):
                decode_metadata(text)
        self.assertEqual(decode_metadata(b'{"version":1}'), {'version': 1})
