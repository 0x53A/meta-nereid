# Recording coverage and current limits

The HAL controller selects29advertised HIDL1 sensor types, preferring their
wakeup variants. Activation success is distinct from fresh output; optical
streams require suitable wearing conditions. SSC discovery currently queries
18explicit names, with13unique endpoints and5empty replies in task0297. It
is not exhaustive enumeration of every firmware feature.

Task0268 captured fresh attributes for all11advertised SSC endpoints and current
sleep configuration. Every reply was matched to the selected source ID, with a
finalized raw journal. Attributes below are firmware claims, not independent
measurements of buffer capacity or system power.

| SSC endpoint | Advertised stream type | Advertised rates/FIFO | Recording state |
|---|---|---|---|
| `accel` | Periodic | 25/50Hz;146samples;data-ready interrupt | Raw samples available through shared HAL |
| `ppg` | Periodic | 25/26Hz;120samples;data-ready interrupt | Raw packed PPG available through HAL; on-wrist validation required |
| `heart_rate` | On change | 1Hz;FIFO0 | Processed HAL channel; freshness depends on wearing/algorithm state |
| `offbody_detect` | On change | Data-ready interrupt; FIFO unspecified | HAL channel; cached values require freshness/debounce |
| `ott` | On change | 200Hz;FIFO0 | LifeQ diagnostic channel; not validated as temperature |
| `fsl_sleep` | On change | FIFO/rates unspecified | Owned profile and getter attachment implemented; task0339 verifies live event2028 delivery, while state mapping/classification remain unverified |
| `fsl_min` | On change | FIFO/rates unspecified | Continuous read-only capture; task0339 separates repeated history from new timestamped motion/step records, while buffered HR and sleep interpretation remain incomplete |
| `fsl_cfg` | On change | FIFO/rates unspecified | Configuration endpoint; logging does not imply permissions enabled |
| `fsl_chrm` | On change | FIFO/rates unspecified | Continuous-heart-rate endpoint present; production activation/collection not integrated |
| `fsl_actrec` | On change | FIFO/rates unspecified | Activity endpoint present; production subscriptions not integrated |
| `fsl_tracker` | On change | FIFO/rates unspecified | Tracker endpoint present; production subscriptions not integrated |

All11advertised AVAILABLE=true in this snapshot. `fsl_usr`, `sleep_wake`, `sac`,
`sensor_temperature`, and `wrist_temperature` returned explicit empty discovery
lists. That means those names were not advertised, not proof that corresponding
hardware or functionality is absent. The separate `fsl_sleep` interface exists.

At25Hz, the advertised physical FIFO counts correspond to4.8s of PPG samples and
5.84s of acceleration samples. Additional hub/software buffering may change how
long the main CPU can sleep; these numbers do not establish total batch capacity,
a guaranteed loss-free interval, or a buffer-full wakeup contract. A missing
FIFO attribute is also not evidence of zero buffering. Advertised microamp
current values describe sensor modes, not whole-watch battery consumption.

Task0268 sleep getters returned exactly the earlier restored baseline: top-level
tracking0 and detect0, with their nested optical settings unchanged. Therefore
minute-buffer retrieval alone must not be called active sleep classification.
Enabling and persisting processed collection still needs owned configuration
lifecycle, restoration on failure, raw/processed time alignment and on-wrist
validation. No sensor configuration was changed in the capability snapshot.

Common recorder readiness, clean stop, crash recovery and discovery selection
are verified on charger. Permanent packaging/session lifecycle, storage rollover,
processed-stream activation/coverage, coordinated CPU suspend and representative
overnight endurance remain incomplete.

Task0284 timestamp refinement: continuous stock-shaped timestamped GET now produces
new minute frames with anchors in the current capture window, while preserving
all old frames. Earlier untimestamped GET trials produced stale anchors. This
narrows the clock issue; timezone units, duplicate/history handling and actual
health measurement freshness/classification remain unverified. Owned sleep
configuration activation/readiness/restoration and recorder failure coupling are
validated in bounded charger trials0280/0282/0284; permanent deployment and sleep
coordination are still incomplete.

Task0285 sleep-event coverage: continuous collection can keep the sleep endpoint
attached using empty tracking/detection queries. Both source-qualified replies were
captured and matched the enabled profile during a combined charger trial; any
subsequent indications would enter the same raw journal. No live sleep-state events
arrived in that trial. Getter attachment is not proof of a subscription contract or
working sleep detection; that remains an explicit validation gap.

Task0288 expanded read-only discovery to47 vendor-schema/stock-library candidates:
29 returned unique endpoints and18 explicit empty lists, with every raw reply
matched independently to the published inventory. These29 SSC endpoints are not
the same inventory as the29 selected HAL types. At that time the default collector used
its16-name list; task0297 expanded it to18; SSC_EXTENDED_DISCOVERY builds are separate research probes with
longer supervision. This is broader coverage, not exhaustive firmware enumeration.

Additional advertised names were gyro,mag,pressure,ambient_light,heart_beat,
pedometer,rotv,game_rv,geomag_rv,gravity,sig_motion,wrist_tilt_gesture,spo2,calories,
rr,fsl_rhr,fsl_wk,fsl_hb_det. Physical/fusion/optical names overlap HAL channels,
but exact cross-interface equivalence is not established merely by their names.

| Newly discovered Fossil endpoint | Firmware attributes | Recording coverage |
|---|---|---|
| fsl_rhr | Resting Heart Rate; available; on change; rates/FIFO unspecified | Periodic read-only snapshots integrated (tasks0289/0290); freshness unverified; HAL resting-HR channel exists separately |
| fsl_wk | Workout Tracker; available; on change; rates/FIFO unspecified | Periodic read-only summaries integrated (task0296); only empty on-charger results validated |
| fsl_hb_det | Heart Beat Detect; available; on change; rates/FIFO unspecified | Direct detector configuration/events not integrated |

Additional empty replies: ambient_temperature,humidity,proximity,pedometer_wrist,
motion_detect,tilt_to_wake,thermopile,hall,rgb,sar,rhr,fsl_hwf,fsl_dvm_tracker.
An empty alias does not disprove a corresponding HAL feature: for example,
motion_detect was empty while HAL motion-detection types were advertised.
No sensors or new algorithm modes were enabled during this inventory probe.

Task0289 fsl_rhr refinement: direct empty query1234 now returns a source-qualified
1029 float snapshot, without threshold configuration. On-charger result0 is raw
and unverified as a measurement. Direct reading is implemented; periodic collection,
freshness and equivalence to HAL basal-RHR are still unverified.

Task0290 continuous RHR refinement: optional direct fsl_rhr reads now share each
minute-buffer collection cycle and journal. Four paired requests/replies were
validated alongside all selected HAL types and sleep-endpoint attachment. Values
were0 on charger; measurement freshness remains unverified. This closes periodic
read integration for this endpoint in bounded trials, not permanent deployment or
onchange/threshold-control validation.

Task0291 storage refinement: continuous SSC journal default128MiB, strict optional
16MiB..1GiB byte budget, fixed256MiB filesystem reserve, explicit policy metadata.
Two live transfers passed and invalid budget failed before file creation. No
rotation/indefinite retention or overnight capacity is claimed; repeated retained
history means archive growth need not remain linear.

Task0292 lifecycle refinement: configuration trial lifetime now uses a
CLOCK_BOOTTIME_ALARM one-shot instead of suspend-paused thread sleep. Live charger
trial verified the timer, expiry and exact configuration restoration. Actual
suspend/wake validation remains outstanding; the180s trial cap is unchanged.

Task0293 extends the native profile lifetime to1..86400s (default30), with apply
service bound max(600,duration+420). Live181s alarm and delayed recovery were
observed; a reboot interrupted original postflight. Durable restoration records
and fresh-boot reads agree with all original settings; all29 journals are clean.
An8h unit was prepared only. Combined recorder limits and actual overnight
endurance are still unverified.

Task0296 fsl_wk refinement: stock-compatible empty779 summary reads implemented
as one-shot and optional reads alongside continuous minute/RHR collection. One
read plus3 repeated cycles returned empty bytes on charger, with clean durable
archives. No workout/state/permission/location activation was sent. Nonempty
summary interpretation, freshness, retention and multipart/>4096-byte snapshot
handling remain unverified.

Task0297 discovery refinement: standard inventory now18 names including fsl_rhr
and fsl_wk; extended inventory stays47. Live standard scan returned13 unique and
5 empty entries with all raw replies verified. Native --select-processed now
selects the4 implemented processed readers together from that standard inventory,
retaining boot/discovery provenance. This remains a queried subset, not exhaustive
SSC enumeration.

Task0301 adds a saved-capture coverage report that includes every selected HAL
channel, including zero-output channels, and separates metadata and unexpected
channels. In task0295, all29 handles activated but only19 produced sensor records;
10 were silent, including PPG. Charging can suppress optical data, and trigger
channels can legitimately be silent. Neither activation nor record count proves
freshness. Task0298 is actively testing eight-hour charger recording; its final
result and representative battery/suspend endurance are not yet established.

Task0303 inspected stock fsl_hb_det: event1028 has required uint64 timestamp and
uint32 ppg/quality. Stock sends configuration request767 and handles767 control
replies. Embedded request schema includes unix_ts_sec/force_stop, but the encoder's
field-presence behavior needs reconciliation with that schema. This is not yet a
proven cached read or a production subscription. Activation/cancellation ownership,
timestamp and physiological interpretation remain unverified; no live request sent.

Task0304 resolves the encoder/schema discrepancy using exact original vendor
protobuf libraries: both SerializeToString implementations use partial serialization
without required-field validation. Stock nonzero timestamp therefore emits field1
alone; zero timestamp leaves an empty request. This is static code evidence, not
firmware acceptance or proof of a read-only request. Heartbeat lifecycle and
cancellation still require investigation before continuous integration.

Task0305 identifies the stock completion marker: nonzero force_stop in reply767;
request timeout returns404 without sending cancellation in that routine. Offline
heartbeat_payload.py preserves complete values and partial/unknown fields without
claiming source, archive completeness or freshness. No live integration yet;
client-release teardown and timeout recovery are still unverified.

Task0298's planned8h charger run failed after about53min: SSC fixed64-slot queue
overflowed. HAL drained cleanly; SSC retained4197durable records,3rejected and64
accepted not confirmed durable. Profile/backend restoration independently verified.
Task0309 repairs queue layout with4096descriptors sharing4MiB payload storage;
512-message blocked-fsync burst and both capacity limits tested. Short on-watch
minute polling passed with98durable records and2complete transfers. Long combined
endurance with this repair remains unverified; prior running status is historical.


Task0315 recovered additional stock interface contracts offline:

- CHRM has disable/continuous/periodic modes and HR+accuracy events. Its stock
  config getter sends empty message775; the schema reply exposes only mode.
  Period, accuracy and callback settings are optional request fields but absent
  from that reply, so full-setting restoration cannot be claimed from it.
- Activity events contain lifecycle state, numeric activity ID and probability;
  configuration includes per-activity latency/threshold. Lifecycle states are
  not activity labels. Subscription and complete restoration remain unverified.
- Tracker requests control SpO2/RHR job states separately from reading their
  configuration. A stock read-config wire candidate was recovered; configuration
  state is not a measured SpO2/RHR value. No new tracker requests were sent.

Schemas, code locations and limitations are in task0315. These are stock-code
findings only and do not expand current live recorder coverage. CHRM's app-side
10-minute logging/clearing interval is not evidence of a 10-minute sensor FIFO.


Task0316 adds explicit diagnostic `--chrm-config` and `--tracker-config` getters
to the SSC helper. Their stock-shaped requests and source-qualified snapshot
handling are host-tested; live acceptance remains unverified. Neither getter is
included in automatic snapshots or continuous collection, and no new algorithm
activation is implemented. Getter availability does not expand measured coverage.

## Overnight evidence, tasks0337–0339

The preserved task0336 wrist recording completed about7.5hours of observed
discharge with clean HAL and SSC finalization and verified configuration
restoration. Task0339 independently matched analyzed event-file hashes to the
watch manifest and cross-checked its transfer reconstruction against the earlier
tracker. This establishes this run's archive integrity and lifecycle, not an8h
endurance claim or measurement accuracy.

- The sleep endpoint delivered20 source-qualified event2028 indications carrying
  raw states0/1. This closes the earlier *no live delivery observed* gap for this
  setup. There were no event1028 indications. The schema defines uint32 state,
  not an enum. Task0340 traces event2028 state unchanged into the stock app's
  WAKE(0), SLEEP(1), DEEP_SLEEP(2) labels. This establishes software interpretation,
  not sleep-classification accuracy or a mapping for event1028.
- Repeated GETs returned274888 file appearances representing539 distinct complete
  file byte sequences:78 present at first read and461 newly observed later.
  New timestamp anchors fall in the recording window. Retained history includes
  an old2021 boot record; reporting that as the current sensor clock is incorrect.
  No ACK/delete was sent. Uniqueness is byte identity, not physiological novelty.
- Newly observed minute activity records contain motion/step fields; their
  decoded step total matches the HAL step-counter increase in this capture.
  Their compact HR fields are all zero, and there are no separate buffered HR
  entries. This differs from the populated live HAL HR stream. The recorded
  nested sleep-HR tracking flag remains disabled; causality is not established.
- Direct fsl_rhr replies stayed zero while separate HAL resting-HR channels
  emitted nonzero values. Do not treat these interfaces as equivalent or interpret
  the direct zero as a measured resting heart rate. Workout summaries stayed empty.
- Packed PPG was retained with a largest source interval of about1.77seconds.
  Clean archive counters do not establish physical sample continuity or decode
  the waveform. Vendor SpO2 zeros/status and RR semantics remain unresolved.

See [task0339](../../../_Tasks/0339_Overnight_Data_Analysis/summary.md) for reproduction
and private analysis/plot locations. No additional sensor configuration was
changed during this offline analysis.
