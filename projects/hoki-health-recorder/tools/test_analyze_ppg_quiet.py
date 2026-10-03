import unittest
import numpy as np
import pandas as pd
from analyze_ppg_quiet import DTYPE, decode_words, quiet_epochs, ppg_spans

class QuietAnalysisTests(unittest.TestCase):
    def test_decoder_bit_mapping(self):
        # Manual permutation: 12 34 56 78 -> 78 34 56 12; then XOR mask.
        self.assertEqual(decode_words([0x12345678])[0], float(0x78345612 ^ 0x0C4507DF))
        self.assertEqual(decode_words([0x00000000])[0], float(0x0C4507DF))

    def test_motion_epochs_distinguish_stationary_from_moving(self):
        rows=np.zeros(250,dtype=DTYPE);rows['time']=np.arange(250)*40_000_000
        xyz=np.zeros((250,3),dtype='<f4');xyz[:,2]=9.81
        xyz[125:,0]=np.sin(np.arange(125)/2)
        rows['words'][:,:3]=xyz.view('<u4')
        result=quiet_epochs(rows,0,.1)
        self.assertEqual(result.quiet.tolist(),[True,False])

    def test_never_bridge_mode_changes_or_data_gaps(self):
        rows=np.zeros(25*200,dtype=DTYPE);rows['time']=np.arange(len(rows))*40_000_000
        rows['time'][25*100:]+=1_000_000_000
        rows['words'][:,1]=1;rows['words'][:,2]=rows['words'][:,4]=0x43000000
        epochs=pd.DataFrame([dict(start_s=0,end_s=205,quiet=True)])
        _,spans=ppg_spans(rows,0,epochs)
        self.assertEqual(len(spans),2)
        self.assertLess(spans[0][1],100);self.assertGreater(spans[1][0],101)
        rows['time']=np.arange(len(rows))*40_000_000
        rows['words'][25*100:,3]=rows['words'][25*100:,6]=1
        _,spans=ppg_spans(rows,0,epochs)
        self.assertEqual(len(spans),2)
        self.assertLess(spans[0][1],100);self.assertGreater(spans[1][0],100)

    def test_offbody_and_nonincreasing_data_not_accepted(self):
        rows=np.zeros(25*70,dtype=DTYPE);rows['time']=np.arange(len(rows))*40_000_000
        epochs=pd.DataFrame([dict(start_s=0,end_s=70,quiet=True)])
        self.assertEqual(ppg_spans(rows,0,epochs)[1],[])
        rows['time'][10]=rows['time'][9]
        with self.assertRaises(ValueError):ppg_spans(rows,0,epochs)

if __name__=='__main__':unittest.main()
