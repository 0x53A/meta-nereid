# Shared raw recording on Hoki

The binder-plugin append includes `0004-shared-raw-recording.patch` for Hoki.
It packages the HIDL1 recorder exercised in tasks0220–0235, including independent
UI/recorder demand arbitration, exact event capture, bounded storage, durable
checkpoints, and the recording wake guard. The sensorfw source revision is still
the upstream recipe's pinned `b889c3d20254e44bc408e64449562c84898506f1`.

This patch builds recorder support into the library. Recording requires an
explicit controller and `HOKI_RECORDING_SOCKET` configuration. It does not itself
start a logging session or control display/radio/suspend policy. HIDL2/FMQ raw
recording is unsupported. See
[`control-protocol.md`](../../../_Tasks/0220_Sensorfw_Recording_Integration/control-protocol.md)
for the private root control protocol.

The image build script synchronizes this layer normally; no hand-copied shared
library is needed to include this source change in a future image. Full component
compilation rebuilds the Binder factory too, which matters because the derived
backend object grew. Earlier object-only tests needed that factory rebuilt
explicitly.

Task0253 verifies both plugin-only and combined local patch stacks against all
six upstream recipe patches, compares the resulting nine recording files with
the tested worktree, and builds the actual BitBake component. Consult that task's
summary for current build results; a recipe patch is not proof of runtime success.
SSC archive/client and long-running recording supervisor integration remain
separate work. The patch is local and has not been submitted upstream.

Task0257 extends the same patch with optional session ownership and bumps the
Hoki package revision to r1.recording2. Modern captures require matching tokens
on mutations; legacy opens retain their previous behavior. Consult0257 for the
updated component build and runtime validation status.


Task0317 updates the local recorder patch and revision to recording3. A terminal
storage error now seals capture input, marks failure, stops recorder demands and
explicitly terminates the recording wake hold. The status includes storage_aborted;
this must never be interpreted as successful stopped/final checkpoint status.
Raw counters and errors remain visible. Unlock or hardware-demand release failures
remain errors requiring the independent recovery supervisor. Guard regression
tests and clean patch stacks pass; component build/live validation are tracked
in task0317. The current wrist recording has not been updated to this backend.
