# Notification sounds

Settings keeps the existing notification and sound switches. The selected sound is either the Windows default notification sound or an imported custom WAV. Preview is an explicit user action; notification playback continues to obey notification preferences.

Imports are validated and copied into clipuh-managed storage so moving the original file does not break the selection. Supported imports are RIFF/WAVE PCM, 8-bit or 16-bit, mono or stereo, 8–192 kHz, no longer than 10 seconds, and no larger than 5 MiB. Unsupported or malformed files leave the prior selection intact. Existing configurations retain the Windows default.

Playback is dispatched outside GPUI through a bounded worker using native Windows sound APIs. It does not run on capture, encoder, or replay export threads. Progress feedback stays silent. Replay clip completion and completed manual recordings use the chosen sound. App playback replaces native balloon audio so background notices do not produce two dings. If a selected custom file is unavailable or Windows cannot play it, playback falls back to the system default. Sounds do not overlap: requests made while a custom sound plays are discarded, and rapid requests within 500 ms are suppressed. Imported managed copies are retained when switching back to Default; original files are never removed.

The supplied request ended at the Project heading. Scope therefore follows the explicit default/custom selection requirement, with file import, preview and persistence. The untracked `clipuh_05_synth_check.wav` in the original checkout is not incorporated without the user's choice about making it the bundled default.

## Verification

Passed workspace formatting, compilation, tests, and strict all-target clippy:

- `cargo fmt --all -- --check`
- `cargo check --workspace`
- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`

Build/check/test commands used an existing shared target directory solely to reuse dependencies. All 156 UI tests passed; targeted tests cover event routing, import cancellation/failure, sound labels, configuration migration/persistence, PCM bounds, malformed chunks and managed import survival after original deletion. Independent review found no important issues.

`cargo test -p clipper-platform hardware_notification_sound_custom_default_and_missing_fallback --lib -- --ignored --nocapture` passed on Windows: the native API accepted a generated 100 ms PCM WAV, and default/missing-file fallback calls completed. API playback success is not proof of audible volume or subjective sound quality.

Native UI automation previously failed to initialize in this environment with `windows sandbox failed: helper_unknown_error: setup refresh had errors`; picker, preview and layout still need interactive acceptance. No subjective listening or gaming-load benchmark was performed.
