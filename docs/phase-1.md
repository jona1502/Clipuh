# Phase 1 architecture and performance

## Boundaries

`clipper-app` owns startup and loads configuration before entering GPUI.
`clipper-ui` owns window state and navigation; `clipper-core` stays independent
of GPUI. Core commands describe intent, backend events confirm completion.
No capture, encoding, audio, replay processing, game detection, tray integration
or hotkey registration is implemented in this phase.

`clipper-storage` owns Serde/TOML settings and per-user configuration paths.
Capture, audio, encoder, replay, platform and editor remain separate library
crates. They intentionally have no multimedia dependencies yet. Future workers
will exchange core commands/events through bounded queues, without coupling
the capture lifecycle to a window.

## Performance decisions

Navigation invalidates the UI only when selection changes. There are no timers,
continuous render requests or capture workers. Configuration I/O belongs before
the UI starts or on a future storage worker, never inside rendering callbacks.
The release profile enables thin LTO and one codegen unit; measure its startup,
size and build-time tradeoffs before changing it.

Future Windows capture should preserve native GPU texture ownership through
capture and hardware encoding, with explicit adapter/device compatibility and
fence synchronization. Avoid CPU readback unless an encoder requires it.
Cross-adapter capture may require copies; zero-copy is a goal, not a guarantee.
NVENC, AMF and Quick Sync capability probing and fallback remain future work.
Bound every frame/packet queue and specify overload behavior before wiring it.
Dropping raw frames is preferable to silently corrupting compressed GOP chains.

At 16 Mbps, 60 seconds of video is approximately 120 MB before audio/container
overhead. A 64 MB RAM budget therefore requires SSD spill in Hybrid mode.
The configured limit must include in-flight allocations and metadata when the
replay engine is implemented; settings alone do not enforce a buffer budget.
Audio routing must exclude game/Discord sessions from desktop remainder capture
before mixing or writing separate tracks, preventing duplicate audio.

## Measurement gates

These are initial targets, not measured claims. Record hardware, GPU driver,
Windows build, power mode, codec and resolution with every result.

- Idle, release build, visible static dashboard: target process CPU below 1%
  averaged over 60 seconds after 10 seconds warmup; report private bytes and GPU
  memory separately. Initial private-byte target: below 150 MiB.
- Navigation: measure input-to-present p50/p95; target p95 below 50 ms across
  100 switches. Verify no steady frame production after interaction settles.
- Future 1080p60 capture: measure capture-to-encoder latency, dropped frames,
  CPU/GPU time and copies per frame over 10 minutes; target p95 below 16.67 ms
  per pipeline stage with zero dropped frames under the documented workload.
- Replay: verify RAM stays within the configured budget under slow SSD and
  overload; measure spill throughput, SSD writes and clip-save p95 latency.
- Audio: test process routing with overlapping sources; each signal must appear
  exactly once. Measure synchronization drift over a 30 minute recording.

## Toolchain and operation

Rust edition 2024; declared minimum Rust 1.96 (conservative foundation baseline).
The checked-in Cargo.lock fixes the resolved application dependency graph.
GPUI 0.2.2 is pinned and enabled only on Windows. Linux GUI support is deferred;
the non-Windows launcher must report that limitation explicitly.

Run `cargo run -p clipper-app --bin clipuh` on Windows (the `clipper-app`
package builds `clipuh.exe`). Quality gates:

```text
cargo fmt --all -- --check
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Building does not prove window rendering, navigation or GPU-driver compatibility.
Runtime validation must be reported separately from compiler checks.
