import gzip,json,tempfile,unittest
from pathlib import Path
from verify_hal import verify,HEADER

class GzipRecording(unittest.TestCase):
    def test_members_checkpoint_tail_crc_and_raw_compatibility(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            data=gzip.compress(HEADER+bytes(88))+gzip.compress(bytes(88))
            p=root/'events-000000.bin.gz';p.write_bytes(data)
            cp=dict(version=1,generation=2,compression='gzip',segment=0,segment_bytes=192,
                    compressed_segment_bytes=len(data),compressed_total_bytes=len(data),total_bytes=192,
                    records=2,dropped=0,input_failures=0,sequence_missing=0,complete=True,final=True)
            (root/'checkpoint.json').write_text(json.dumps(cp))
            self.assertTrue(verify(root)['checkpoint_claim_and_bytes_consistent'])
            from health_decode import summarize
            summarize(root)
            p.write_bytes(data+b'\x1f\x8b\x08\x00')
            result=verify(root)
            self.assertEqual(result['durable_records'],2)
            self.assertFalse(result['checkpoint_claim_and_bytes_consistent'])
            self.assertEqual(result['files'][0]['unacknowledged_tail_bytes'],4)
            damaged=bytearray(data);damaged[-8]^=1;p.write_bytes(damaged)
            with self.assertRaises((OSError,EOFError,ValueError)):verify(root)
