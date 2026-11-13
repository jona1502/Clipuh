# Storage estimates

The dashboard estimates encoded file sizes from the same targets used by the
recorder. It does not measure files, disk space or replay-buffer occupancy.

## Rate sources

- Video uses `H264Tuning::new` with the configured quality profile, base bitrate
  and FPS. Performance and Balanced retain the base rate; Quality applies the
  existing 1.5x target and cap. Resolution and FPS do not multiply a known rate.
- Audio uses the shared `AAC_BITRATE_BPS` constant and
  `RecordingAudioOptions::encoded_track_count()`. The current recorder creates
  one mix when desktop or microphone is enabled, plus an isolated track for each
  enabled source when separate tracks are enabled. Game/Discord already belong
  to the desktop mix; their future process preferences do not add tracks.
- H.264 and AAC are the only implemented encoding formats. Unsupported codecs
  are not presented as available features. AAC remains fixed at 192 kbps/track.

`clipper-core::storage_estimate` performs checked `u128` arithmetic and converts
back to `u64` bytes. Payload bits and optional positive overhead fractions round
up to complete bytes. Generic zero rates or durations yield zero; application
configuration validation rejects values the encoder does not support.

The UI uses decimal MB/GB. RAM configuration is interpreted as MiB, matching its
existing validation contract. Hybrid capacity estimates use `min(total, budget)`
for RAM and `max(total - budget, 0)` for SSD. These are theoretical allocations;
segment metadata, GOP boundaries and live occupancy can change actual use.

## Update and accuracy limits

Estimates refresh from configuration changes, not timers or disk queries. The
UI caches relevant inputs and retains an accepted recording snapshot separately
from settings for the next recording. No capture/audio/replay worker is started
by the calculator. MP4 overhead defaults to zero and is explicitly excluded.
VBR results describe target-rate estimates, not guaranteed file sizes.

At defaults, 16 Mbps video plus three 192 kbps AAC tracks gives 124.32 MB/min.
Quality gives 184.32 MB/min. A single mixed AAC track at the default video rate
uses 121.44 MB/min. A 60-second default replay projection with a 64 MiB budget
estimates 57.21 MB on SSD; this is not a measurement of a running replay engine.

Windows GUI interaction still needs manual validation: the native computer-use
connection was unavailable during implementation. Unit tests cover cache
updates, accepted/active settings, exact byte counts and display formatting.
