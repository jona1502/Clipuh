# Notification sounds

Settings keeps the existing notification and sound switches. The clip-saved sound is either clipuh's default sound or an imported custom sound. Preview is an explicit user action; notification playback continues to obey notification preferences.

The default sound is `assets/sounds/clipuh_05_synth_check.wav`, the approved file, unmodified. It is embedded in the executable, so it works offline and needs no installed files. If a selected custom file is unavailable or Windows cannot play it, the default plays instead; the Windows system sound is only the last resort when even the bundled sound cannot play.

The clip-saved sound plays only when a clip was saved and finalised (the recorder's `ReplaySaved` event) and for completed manual recordings. Pressing the hotkey, a queued save and the "Saving…" toast are silent. A failed save plays the system error sound instead, never the clip-saved sound.

Imports accept WAV and MP3. MP3 files, and WAVs that are not plain PCM (for example 32-bit float), are decoded with Media Foundation (the decoder the editor uses) into a 16-bit 48 kHz stereo WAV first: at most 10 seconds, not silent, peaks limited to -1 dBFS and never amplified. OGG is not offered because Windows has no built-in Vorbis decoder. Every import is then validated and copied into clipuh-managed storage (PCM 8/16-bit, mono or stereo, 8–192 kHz, at most 10 seconds and 5 MiB), so moving the original file does not break the selection. Unsupported or malformed files leave the prior selection intact. Existing configurations get the clipuh default sound.

Playback is dispatched outside GPUI through a bounded worker using native Windows sound APIs. It does not run on capture, encoder, or replay export threads. App playback replaces native balloon audio so background notices do not produce two dings. Sounds do not overlap: requests made while a sound plays are discarded, and rapid requests within 500 ms are suppressed. Imported managed copies are retained when switching back to Default; original files are never removed.

## Verification

Passed workspace formatting, compilation, tests, and strict all-target clippy:

- `cargo fmt --all -- --check`
- `cargo check --workspace`
- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`

Build/check/test commands used an existing shared target directory solely to reuse dependencies. All 156 UI tests passed; targeted tests cover event routing, import cancellation/failure, sound labels, configuration migration/persistence, PCM bounds, malformed chunks and managed import survival after original deletion. Independent review found no important issues.

`cargo test -p clipper-platform hardware_notification_sound_custom_default_and_missing_fallback --lib -- --ignored --nocapture` passed on Windows: the native API accepted a generated 100 ms PCM WAV, and default/missing-file fallback calls completed. API playback success is not proof of audible volume or subjective sound quality.

Native UI automation previously failed to initialize in this environment with `windows sandbox failed: helper_unknown_error: setup refresh had errors`; picker, preview and layout still need interactive acceptance. No subjective listening or gaming-load benchmark was performed.
