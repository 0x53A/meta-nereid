# Build and install

From this directory, use the shared Rust workspace and pinned Slint dependencies:

```sh
nix-shell --arg nativeOnly true --run 'cargo test --locked -p hoki-activity'
nix-shell --arg nativeOnly true --run 'HOKI_ACTIVITY_TEST_CAPTURES=/tmp/activity-previews cargo test --locked -p hoki-activity watch_actions'
nix-shell --run 'cargo build --locked --release --target armv7-unknown-linux-gnueabihf -p hoki-activity'
```

Run the repository ELF patch helper before direct deployment. The Asteroid SDK
is also supported as a cross linker; record the SDK/sysroot used with artifacts.
`hoki-ui` builds the binary from the shared workspace and emits a separate
`hoki-activity` package containing its launcher, Python service and desktop entry.
Slint, zbus and the Python standard library are used for this app. The separately
installed system GeoClue provider still uses Qt; the UI and bridge do not.

The launcher runs the GUI as ceres. Its user service owns the activity and starts
on first UI launch. Install the coordinated health-policy/controller and SpO2 app
changes together. Preserve and finalize any existing capture before migration;
initial recorder setup still restarts sensorfw. `deploy/stage.py` stages and hashes
the coordinated payload from three supplied ARM binaries without deploying it.

Host-only checks and export:

```sh
python3 -m unittest discover -s tests -v
python3 export.py /path/to/activity.jsonl /path/to/new-export
```

`export.py` creates `.gpx` and `.csv` and refuses existing output files. It does
not transmit data. Copy the activity journal and referenced HAL capture with
SSH/SCP for desktop analysis. Raw GPS messages are embedded in the activity
journal; the Rust bridge does not create a second GPS archive.
