#!/usr/bin/env python3
"""Offline exploratory quiet-window PPG analysis. Does not classify sleep.

Dependencies: neurokit2==0.2.12, numpy, scipy, pandas, matplotlib.
Packed decoder provenance: _Tasks/20260924_Sensor_Firmware_Recovery/
ppg_firmware_decode.py and array-routing.md (ADSP 0x2db369e8).
HAL optical colour/units and interval accuracy remain unvalidated.
"""
import argparse
import base64
import html
import json
import warnings
from pathlib import Path

import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
import neurokit2 as nk
import numpy as np
import pandas as pd

from recording_io import decoded_segment
from verify_hal import verify

DTYPE = np.dtype([('arrival','<i8'),('time','<i8'),('handle','<u4'),
                  ('type','<u4'),('words','<u4',(16,))])
FS = 25


def decode_words(raw):
    raw = np.asarray(raw, dtype=np.uint32)
    out = np.zeros_like(raw)
    for source, destination in enumerate((3,1,2,0)):
        out |= ((raw >> (source*8)) & 255) << (destination*8)
    return (out ^ np.uint32(0x0C4507DF)).view(np.int32).astype(float)


def load_channels(root, checkpoint):
    groups = {t: [] for t in (1,21,65572,65574)}
    for i in range(checkpoint['segment']+1):
        raw=decoded_segment(root,checkpoint,i)
        size=checkpoint['segment_bytes'] if i==checkpoint['segment'] else len(raw)
        data=np.frombuffer(raw,dtype=DTYPE,offset=16,count=(size-16)//88)
        for typ in groups:
            groups[typ].append(data[data['type']==typ].copy())
    channels={}
    for typ, parts in groups.items():
        rows=np.concatenate(parts)
        if not len(rows):continue
        handles,counts=np.unique(rows['handle'],return_counts=True)
        # Do not merge wakeup and non-wakeup duplicate streams.
        channels[typ]=rows[rows['handle']==handles[np.argmax(counts)]]
    if not {1,65572}.issubset(channels):raise ValueError('PPG and acceleration required')
    return channels


def quiet_epochs(acc, origin, threshold):
    t=(acc['time']-origin)/1e9
    xyz=acc['words'][:,:3].copy().view('<f4').astype(float)
    if np.any(np.diff(t)<=0):raise ValueError('Non-increasing acceleration timestamps')
    rate=1/np.median(np.diff(t));bins=np.floor(t/5).astype(int)
    rows=[]
    for b in range(max(0,bins.min()),bins.max()+1):
        x=xyz[bins==b];times=t[bins==b]
        coverage=len(x)/(5*rate)
        complete=len(x)>1 and coverage>=.8 and np.max(np.diff(times))<=.25
        rms=float(np.sqrt(np.mean(np.sum((x-x.mean(axis=0))**2,axis=1)))) if len(x) else float('nan')
        rows.append(dict(start_s=b*5,end_s=(b+1)*5,rms_m_s2=rms,
                         coverage=coverage,quiet=bool(complete and np.isfinite(rms) and rms<threshold)))
    return pd.DataFrame(rows)


def runs(mask):
    edges=np.diff(np.r_[False,mask,False].astype(int))
    return list(zip(np.flatnonzero(edges==1),np.flatnonzero(edges==-1)))


def ppg_spans(ppg, origin, epochs, gap=.12):
    t=(ppg['time']-origin)/1e9;w=ppg['words'];mode=(w[:,3]!=0)|(w[:,6]!=0)
    # Recovered duplicate body fields provide cached firmware contact evidence.
    valid=(w[:,2]==0x43000000)&(w[:,4]==0x43000000)&(w[:,1]!=0)
    valid &= ((w[:,3]==0)==(w[:,6]==0))
    if np.any(np.diff(t)<=0):raise ValueError('Non-increasing PPG timestamps')
    cuts=np.r_[0,np.flatnonzero((np.diff(t)>gap)|(mode[1:]!=mode[:-1])|
              (valid[1:]!=valid[:-1]))+1,len(t)]
    spans=[]
    for left,right in zip(cuts[:-1],cuts[1:]):
        if right-left<2 or not np.all(valid[left:right]):continue
        for q0,q1 in runs(epochs.quiet.to_numpy()):
            start=max(t[left],float(epochs.iloc[q0].start_s))+2
            end=min(t[right-1],float(epochs.iloc[q1-1].end_s))-2
            # Keep disjoint windows <=5min; never stitch across motion or gaps.
            while end-start>=60:
                stop=min(start+300,end)
                spans.append((start,stop,left,right,bool(mode[left])))
                start=stop
    return t,spans


def process(root, output, threshold, polarity=1):
    output.mkdir(parents=True,exist_ok=False)
    integrity=verify(root)
    cp=integrity['checkpoint']
    if not cp['complete'] or not cp['final'] or any(cp[k] for k in ('dropped','input_failures','sequence_missing')):
        raise ValueError('Require a finalized loss-free capture for this analysis')
    (output/'integrity.json').write_text(json.dumps(integrity,indent=2))
    channels=load_channels(root,cp);acc=channels[1];ppg=channels[65572]
    origin=min(int(acc['time'][0]),int(ppg['time'][0]))
    epochs=quiet_epochs(acc,origin,threshold);epochs.to_csv(output/'motion.csv',index=False)
    t,spans=ppg_spans(ppg,origin,epochs)
    decoded=decode_words(ppg['words'][:,1])
    hr=channels.get(21,np.empty(0,dtype=DTYPE))
    hr_t=(hr['time']-origin)/1e9;hr_v=hr['words'][:,0].copy().view('<f4')
    rows=[];pulse_rows=[];examples=[];notices=[]
    for idx,(start,end,left,right,mode) in enumerate(spans):
        grid=np.arange(start,end,1/FS)
        raw=polarity*np.interp(grid,t[left:right],decoded[left:right])
        row=dict(segment=idx,start_s=start,end_s=end,duration_s=end-start,
                 extra_optical_positions=mode,channel_word=1,polarity=polarity)
        if np.ptp(raw)==0:
            row.update(status='flat_signal',accepted=False);rows.append(row);continue
        try:
            with warnings.catch_warnings(record=True) as caught:
                warnings.simplefilter('always')
                signals,info=nk.ppg_process(raw,sampling_rate=FS,method='elgendi',method_quality='templatematch')
            notices.extend(dict(segment=idx,message=str(w.message)) for w in caught)
            peaks=np.asarray(info['PPG_Peaks'],dtype=int)
            # Exclude filter-edge detections from interval calculations.
            peaks=peaks[(peaks>=3*FS)&(peaks<len(raw)-3*FS)]
            times=grid[peaks];ibi=np.diff(times)*1000
            quality=signals['PPG_Quality'].to_numpy()
            q=float(np.nanmedian(quality[3*FS:-3*FS]))
            plausible=(ibi>=300)&(ibi<=2000)
            outlier=(np.abs(ibi-np.median(ibi))>.2*np.median(ibi)) if len(ibi) else np.ones(0,dtype=bool)
            fraction=float(np.mean(plausible & ~outlier)) if len(ibi) else 0.
            accepted=bool(len(peaks)>=30 and q>=.8 and fraction>=.95)
            stock=hr_v[(hr_t>=start)&(hr_t<=end)&(hr_v>0)&np.isfinite(hr_v)]
            row.update(status='processed',accepted=accepted,pulses=len(peaks),quality_median=q,
                       regular_interval_fraction=fraction,hr_bpm=60000/float(np.median(ibi)) if len(ibi) else None,
                       stock_hr_bpm=float(np.median(stock)) if len(stock) else None,
                       stock_hr_reports=len(stock),raw_rmssd_ms=float(np.sqrt(np.mean(np.diff(ibi)**2))) if len(ibi)>1 else None,
                       raw_sdnn_ms=float(np.std(ibi,ddof=1)) if len(ibi)>1 else None)
            # NK metrics retained as exploratory PRV, not clinical HRV or sleep staging.
            if len(peaks)>3:
                with warnings.catch_warnings(record=True) as caught:
                    metrics=nk.hrv_time(peaks,sampling_rate=FS,show=False).iloc[0]
                row['neurokit_rmssd_ms']=float(metrics['HRV_RMSSD'])
                row['neurokit_sdnn_ms']=float(metrics['HRV_SDNN'])
                notices.extend(dict(segment=idx,message=str(w.message)) for w in caught)
            pd.DataFrame({'time_s':grid,'decoded_word1':raw,'cleaned':signals.PPG_Clean,
                          'quality':quality,'peak':np.isin(np.arange(len(grid)),peaks)}).to_csv(output/f'segment-{idx:03}.csv.gz',index=False)
            for k,stamp in enumerate(times):
                pulse_rows.append(dict(segment=idx,pulse=k,time_s=stamp,interval_ms=ibi[k-1] if k else None))
            examples.append(dict(row=row,time=grid,raw=raw,clean=signals.PPG_Clean.to_numpy(),peaks=peaks,quality=quality))
        except (ValueError,IndexError,ZeroDivisionError) as exc:
            row.update(status='algorithm_error',accepted=False,error=str(exc))
        rows.append(row)
    table=pd.DataFrame(rows);table.to_csv(output/'segments.csv',index=False)
    pd.DataFrame(pulse_rows).to_csv(output/'pulses.csv',index=False)
    accepted=table[table.accepted] if len(table) else table
    summary=dict(capture=str(root.resolve()),origin_boottime_ns=origin,neurokit_version=nk.__version__,
                 ppg_records=len(ppg),acc_records=len(acc),duration_minutes=(t[-1]-t[0])/60,
                 quiet_minutes=float(epochs.quiet.sum()*5/60),motion_threshold_m_s2=threshold,
                 epochs_seconds=5,gap_limit_seconds=.12,resampled_hz=FS,minimum_segment_seconds=60,
                 segments=len(rows),processed=sum(r['status']=='processed' for r in rows),
                 accepted_segments=len(accepted),accepted_minutes=float(accepted.duration_s.sum()/60) if len(accepted) else 0,
                 sleep_label='not available; low movement is not proof of human sleep',
                 quality_rule='median template similarity>=0.8, >=30pulses, >=95% intervals300..2000ms and within20% of segment median',
                 limitations=['unvalidated heuristic quality thresholds',f'unknown physical optical units/colour; decoded word1, polarity {polarity}',
                              'PPG-derived PRV, no ECG reference','25Hz grid has40ms peak-time spacing; no claim of finer timing',
                              'motion RMS about local mean removes gravity but also slow orientation changes',
                              'interpolation only within continuous source segments; no filled optical gaps'],warnings=notices)
    (output/'summary.json').write_text(json.dumps(summary,indent=2,allow_nan=False))
    fig,axes=plt.subplots(3,1,figsize=(13,9),sharex=True,constrained_layout=True)
    axes[0].plot(epochs.start_s/60,epochs.rms_m_s2,lw=.7,color='#556677')
    axes[0].axhline(threshold,color='orange',ls='--',label='Quiet threshold')
    axes[0].set(yscale='log',ylabel='Acceleration RMS (m/s²)',title='Quiet PPG analysis — no verified sleep labels')
    for r in rows:
        colour='#70bf87' if r.get('accepted') else '#f0b367'
        axes[0].axvspan(r['start_s']/60,r['end_s']/60,color=colour,alpha=.3)
    axes[0].legend(loc='upper right')
    valid=(hr_v>0)&np.isfinite(hr_v);axes[1].plot(hr_t[valid]/60,hr_v[valid],color='#888888',lw=1,label='Stock HR (cross-check)')
    for r in rows:
        if r['status']=='processed':
            axes[1].plot([(r['start_s']+r['end_s'])/120],[r['hr_bpm']],'o',color='#19734a' if r['accepted'] else '#d58425')
            axes[2].plot([(r['start_s']+r['end_s'])/120],[r['raw_rmssd_ms']],'o',color='#19734a' if r['accepted'] else '#d58425')
    axes[1].set(ylabel='HR / pulse rate (bpm)');axes[1].legend()
    axes[2].set(ylabel='Exploratory pulse RMSSD (ms)',xlabel='Minutes from capture start')
    for ax in axes:ax.grid(alpha=.2)
    fig.savefig(output/'overview.png',dpi=160);fig.savefig(output/'overview.pdf');plt.close(fig)
    chosen=next((e for e in examples if e['row'].get('accepted')),examples[0] if examples else None)
    if chosen:
        grid=chosen['time'];left=min(10,len(grid)/FS/4);mask=(grid>=grid[0]+left)&(grid<grid[0]+left+15)
        fig,ax=plt.subplots(figsize=(13,3.5),constrained_layout=True)
        ax.plot(grid[mask]-grid[0],chosen['clean'][mask],label='NeuroKit cleaned PPG')
        peaks=chosen['peaks'];peaks=peaks[mask[peaks]]
        ax.scatter(grid[peaks]-grid[0],chosen['clean'][peaks],color='red',s=25,label='Detected pulses')
        ax.set(xlabel='Seconds within segment',ylabel='Decoded signal (arbitrary units)',title=f"Segment {chosen['row']['segment']}: pulse detections")
        ax.legend();ax.grid(alpha=.2);fig.savefig(output/'pulses.png',dpi=160);plt.close(fig)
    pictures=''.join('<img style="width:100%" src="data:image/png;base64,'+base64.b64encode((output/f).read_bytes()).decode()+'">' for f in ('overview.png','pulses.png') if (output/f).exists())
    explanation='Low activity only; no verified sleep labels. Green passes exploratory waveform/interval quality checks; orange does not. PRV values are not validated ECG HRV. The provisional regularity screen can reject real variability; passing does not establish beat-timing accuracy. Decoded word1 (polarity recorded per segment), 25Hz grid; no interpolation across optical gaps.'
    page='<html><meta charset="utf-8"><title>Quiet PPG analysis</title><body style="font-family:system-ui;max-width:1200px;margin:2em auto"><h1>Quiet PPG analysis</h1><p>'+explanation+'</p><pre>'+html.escape(json.dumps({k:v for k,v in summary.items() if k not in ('warnings','limitations')},indent=2))+'</pre>'+pictures+table.to_html(index=False,float_format=lambda x:f'{x:.2f}')+'</body></html>'
    (output/'report.html').write_text(page)
    print(json.dumps({k:v for k,v in summary.items() if k!='warnings'},indent=2))


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('capture',type=Path);parser.add_argument('output',type=Path)
    parser.add_argument('--motion-threshold',type=float,default=.1)
    parser.add_argument('--polarity',type=int,choices=(-1,1),default=1)
    args=parser.parse_args();process(args.capture,args.output,args.motion_threshold,args.polarity)
