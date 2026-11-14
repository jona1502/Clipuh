# Capture mixer gain and noise gate

Double-click a source gain label to edit dB. Enter commits; Escape cancels. Leaving the field commits a valid number or preserves the current gain and reports invalid input. Decimal commas and points are accepted. The slider and text field both commit to the existing integer percent gain setting; dB values are rounded to that engine precision. Zero percent is silence, displayed as negative infinity. The engine maximum remains 200 percent (approximately +6.02 dB).

Each source has an optional noise gate, disabled for existing configurations. Threshold is adjustable from -80 to 0 dBFS, attack from 1 to 100 ms, hold from 0 to 1000 ms, and release from 5 to 2000 ms. Defaults are -45 dBFS, 5 ms attack, 100 ms hold, and 150 ms release.

Processing order is capture/resampling and timestamp placement, then the per-source gate, then source gain and mute/solo ramping, then the existing mix and isolated-track limiters. The detector measures the source before user gain, so changing gain does not change the threshold. Channels share a detector and attenuation to preserve stereo balance. Recording, replay, and diagnostic WAV output use the same mixer; source identity and output track topology are unchanged.

The gate attenuates quiet periods. It does not remove fan noise or hiss while speech keeps the gate open. Start with the default threshold, then lower it if quiet speech is cut off. Increase hold or release if pauses sound abrupt.

The processor operates on the existing 48 kHz timeline, with bounded state, no lookahead, no additional buffering, and no sample-loop allocation or I/O. Attack smooths the start of a sound; there is no lookahead to recover its first milliseconds. Live bypass ramps back to unity using the attack time to avoid a sudden discontinuity. A session with gating disabled preserves the existing path. Source reconnection starts fresh gate state. Optional live open/closed decoration is omitted; no simulated gate activity is shown.

## Verification record

Automated checks and actual hardware observations for this change are recorded in the PR. Synthetic transition and timestamp tests do not establish acoustic quality or game performance.

Initial Windows hardware checks: microphone capture delivered 47,520 frames with zero drops; desktop loopback tone capture delivered 168,960 frames with zero drops. The pre-existing application/desktop exclusion test failed because the tone appeared in the desktop remainder and the selected application track remained silent. A separate concurrent microphone/desktop gate test verified identical block timestamps and sample counts, unchanged desktop samples, live bypass returning to unity, and zero capture drops. With the corrected detector envelope, at a deliberately high -20 dBFS threshold (0 ms hold, 20 ms release), measured quiet microphone energy fell from 0.05503420 to 0.00431367 in the sampled window (about 92%). This is ambient-noise evidence, not a speech-intelligibility assessment. Diagnostic WAV files remain local and are not committed.

Manual acceptance should include two simultaneous sources, quiet and loud speech, keyboard/fan noise between phrases, live toggles during recording and replay, microphone disconnect/restart, Enter/Escape/focus behavior, invalid numeric input, and persistence after app restart. Listen to actual output before judging speech quality. Measure capture FPS and CPU under the intended game workload before claiming no gaming performance regression.

All required final checks passed: `cargo fmt --all -- --check`, `cargo check --workspace`, `cargo test --workspace`, and `cargo clippy --workspace --all-targets -- -D warnings`. Hardware tests remain opt-in. The existing microphone and desktop tone tests passed; the existing application exclusion tone test failed as described above.

`cargo run -p clipper-recorder --example replay_smoke -- run=6 duration=5 fps=30 desktop=1 mic=1 gate=1 separate=1` exercised microphone and desktop recording with live gate disable/re-enable during replay, saving four MP4 clips. All four exports completed, replay stopped without error, and diagnostics reported zero audio glitch frames and zero invalid packets. This short desktop run is not a gaming workload or a subjective synchronization assessment.

`cargo test -p clipper-audio --release dsp_cost -- --ignored --nocapture` measured four stereo sources over 60 seconds of synthetic audio, minimum of three runs: bypass 99.3352 ms, gated 100.7084 ms. Full gated mixer cost was about 0.168% of one CPU core in real-time terms; incremental gate cost was about 0.0023%. No buffering was added. These wall-clock microbenchmark results do not establish application latency or gaming FPS under load.

Native UI automation could not start: the Windows tool runtime exited with `windows sandbox failed: helper_unknown_error: setup refresh had errors`. Double-click/focus behavior and minimum-width visual fit therefore still require interactive acceptance. Parser, percent round-trips, selection replacement, configuration persistence and DSP transitions have automated coverage. No speech listening test or end-to-end latency measurement was performed. `ffprobe` was unavailable on PATH, so a separate CLI inspection of the smoke MP4 metadata was not performed.
