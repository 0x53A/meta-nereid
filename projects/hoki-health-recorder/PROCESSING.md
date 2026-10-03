# Processing baseline and next implementation

Planning decision, 2026-09-28: build processing around approximately 25 Hz raw
PPG and timestamped accelerometer data. Higher-rate optical acquisition is an
independent investigation, not a prerequisite. This document specifies planned
work; it does not describe an implemented waveform-analysis pipeline.

## Input contract

- Preserve original packed PPG records, source/arrival timestamps, channel
  descriptors, optical transitions and activity markers. Decode with the existing
  evidence-backed positional transform; do not invent LED labels.
- Estimate cadence from source timestamps in each continuous segment. The Hoki
  experiments delivered about25.4–25.7HAL records/s despite requests up to100Hz.
  Treat25Hz as a planning scale, never replace recorded time with sample_index/25.
  Per-optical-lane timing must be confirmed when integrating the packed decoder.
- Align acceleration by source time. Running requests50Hz acceleration; collection
  rates remain independent. Use timestamps to compute motion features over PPG
  windows; do not assume an integer samples-per-pulse ratio.
- Split at optical-mode changes, timestamp discontinuities and missing data.
  Exclude startup settling windows. Retain rejected data and explicit reasons.
  Do not join across ~0.7second optical transitions or fabricate absent beats.

## First processing increment

Export a local analysis dataset with decoded positional PPG, motion, processed
HR/RR references and markers. Plot raw/cleaned PPG, estimated pulse times, motion
and quality on one timeline. Compare pulse-derived HR with existing processed HR;
use processed RR as a cross-check, not independent ground truth. Keep derived
outputs separate and version the decoder, parameters and quality rules.

Implement pulse detection and a quality score before summary metrics. Report
missing/uncertain intervals explicitly. Subsample peak fitting is a hypothesis
to validate against a suitable reference; interpolation alone does not establish
additional timing accuracy at roughly40ms sample spacing.

## Sleep and quiet-window analysis

Use on-wrist evidence, low motion and PPG waveform quality to select contiguous
windows (initial target: five minutes). Stillness is a signal-quality criterion,
not sufficient evidence of sleep. Keep sleep-bout inference separate from pulse
analysis and retain quiet-awake periods as such when labels are available.

For accepted windows, compute pulse-interval metrics such as RMSSD, median HR,
window coverage and rejection counts. Call the optical measurement pulse-rate
variability (PRV); validate its agreement with ECG-derived HRV before presenting
it as interchangeable. Establish beat-timing error and usable coverage with a
synchronized reference, including rest, sleep and motion; do not infer validity
from good average-HR agreement. Keep the overnight timeline/distribution, not
only a single unqualified nightly number. Withhold metrics for poor windows.

Current running and sleep profiles do not select raw PPG. Add a purpose-built
PPG-capable subscription/profile through the daemon for this work; using Full
for early captures is already possible but also enables unrelated high-rate
channels. Respect existing SpO2/beat exclusivity and annotate optical windows.
This profile extension and processing implementation remain pending.

## Outputs

Keep raw data, beat/pulse intervals, quality and analysis provenance in local
exports. Activity exports can carry derived HR/cadence and future GPS metrics;
external fitness services are downstream consumers, not raw-PPG processors.
Sleep export should include window boundaries and valid-data coverage alongside
metrics. No automatic health-data upload is part of this plan.

## Higher-rate investigation

The shipped and persisted PPG profile lists reference only LifeQ25/two-position
and LifeQ26/four-position configurations; firmware timing for both is25Hz.
A separate HRD.QC_100hz_2ch configuration is present but absent from those lists.
Its availability, request routing, algorithm compatibility, power cost and optical
quality must be established before considering a different processing baseline.

## First offline implementation (2026-09-28)

`tools/analyze_ppg_quiet.py` now runs NeuroKit2 0.2.12's Elgendi pipeline over
accelerometer-selected quiet segments of a finalized capture. Install dependencies
from `tools/requirements-ppg.txt` in a separate Python environment, then run:

```
python tools/analyze_ppg_quiet.py /path/to/capture/hal /new/private/output --polarity=-1
```

Output includes a standalone HTML report, PNG/PDF plots, motion epochs, segment
metrics, detected pulses, processed per-segment signal CSVs and integrity metadata.
The analysis never writes into the input capture or contacts the watch.

The first inspected30.5-minute capture yielded8segments,24.69minutes after
segmentation, and3windows/9.2minutes passing an exploratory quality screen.
Inverting decoded word1 before processing substantially reduced missed peaks;
positive-polarity results are retained as a diagnostic comparison. Polarity is
explicit, not silently selected by agreement with stock HR. The three selected
windows gave median pulse rates50.0,51.7,50.0bpm versus stock49,52,50bpm.
These are quiet windows without human-sleep labels, not a validated sleep study.

Quality thresholds are provisional: template similarity>=0.8, >=30pulses,
>=95% intervals within300..2000ms and within20% of the segment median. This
regularity screen can reject real physiological variability; passing does not
establish HRV accuracy. Reported RMSSD/SDNN are uncorrected exploratory optical
PRV at40ms peak-grid spacing, not clinical or overnight HRV summaries. No beat
interpolation, automatic artifact correction or physiological interpretation is
added. Broad/split pulse maxima remain a timing-accuracy concern.
