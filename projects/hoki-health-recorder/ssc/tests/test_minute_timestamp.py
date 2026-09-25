from pathlib import Path
import struct
import unittest
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import minute_timestamp as m
class TimestampTests(unittest.TestCase):
 def event(self, kind, seconds=1789977888, ms=567, zone=-18000):
  return bytes([0xe2,kind])+struct.pack('<IHh',seconds,ms,zone)
 def test_timestamp_only_and_signed_zone(self):
  d=m.decode(self.event(4));self.assertEqual(d['stock_timestamp_ms'],1789977888567)
  self.assertEqual(d['timezone_word'],-18000);self.assertIsNone(d['info_code'])
  self.assertEqual(d['stock_minute_second'],48);self.assertTrue(d['timestamp_only'])
 def test_hardware_reset_has_no_timestamp(self):
  d=m.decode(self.event(2));self.assertEqual(d['info_name'],'HARDWARE_RESET')
  self.assertIsNone(d['stock_timestamp_ms']);self.assertIsNone(d['stock_minute_second'])
 def test_info_and_unknown_preserved(self):
  for k,n in m.INFO_CODES.items():self.assertEqual(m.decode(self.event(k))['info_name'],n)
  d=m.decode(self.event(255,4294967295,65535,32767))
  self.assertEqual(d['stock_timestamp_ms'],4294967360535);self.assertIsNone(d['info_name'])
  self.assertFalse(d['fraction_in_standard_range']);self.assertFalse(d['fresh_measurement_verified'])
 def test_reject_wrong_shape(self):
  for raw in [b'',self.event(1)[:-1],self.event(1)+b'\0',b'\xe1'+self.event(1)[1:]]:
   with self.assertRaises(ValueError):m.decode(raw)
if __name__=='__main__':unittest.main()
