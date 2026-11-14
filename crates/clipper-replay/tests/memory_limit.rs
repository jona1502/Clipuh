//! Lowering the RAM budget of a live hybrid session must move footage to the
//! SSD, not throw the whole buffer away.

mod common;

use std::time::Duration;

use clipper_replay::disk::DiskConfig;
use clipper_replay::hybrid::{BufferTier, ReplayBuffer, ReplayConfig};
use common::*;

#[test]
fn lowering_memory_limit_keeps_footage_on_disk() {
    let root = TempDir::new("shrink-limit");
    let mut synth = Synth::new(1, 16_000_000);
    let mut buf = ReplayBuffer::new(
        ReplayConfig {
            duration: Duration::from_secs(120),
            memory_limit_bytes: 64 * MIB,
            disk: Some(DiskConfig {
                root: root.path().to_path_buf(),
                max_bytes: 512 * MIB as u64,
            }),
        },
        layout(1, 1920, 1080),
    )
    .unwrap();
    // Feeds one GOP, then waits until the spill thread has brought RAM back
    // under `limit`, as real-time capture gives it time to (the synthetic
    // stream is far faster than real time). Waiting on the buffer's own state
    // instead of a fixed sleep keeps the test independent of disk speed (a
    // fixed 150 ms flaked on slow CI runners).
    let gop = |synth: &mut Synth, buf: &mut ReplayBuffer, limit: usize| {
        let next = synth.frames() + GOP_FRAMES;
        synth.run_frames(next, |p| buf.push(p)).unwrap();
        assert!(
            wait_for(Duration::from_secs(20), || buf.stats().ram_bytes as u64
                <= limit as u64),
            "spilling did not catch up: {:?}",
            buf.stats()
        );
    };
    // 40 s of footage (about 76 MiB, so the SSD tier is already in use).
    for _ in 0..20 {
        gop(&mut synth, &mut buf, 64 * MIB);
    }
    let before = buf.stats();
    assert!(before.buffered >= Duration::from_secs(38));
    assert_eq!(before.dropped_gops, 0);

    buf.set_memory_limit(4 * MIB);
    for _ in 0..10 {
        gop(&mut synth, &mut buf, 4 * MIB);
    }
    assert!(wait_for(Duration::from_secs(10), || {
        buf.stats().tier == BufferTier::Hybrid
    }));
    let after = buf.stats();
    assert_eq!(after.dropped_gops, 0, "footage was discarded: {after:?}");
    assert!(
        after.buffered >= Duration::from_secs(58),
        "buffered only {:?}",
        after.buffered
    );
    assert!(after.disk_bytes > 0);
}
