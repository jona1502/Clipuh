//! Clip-saved notification sound: the bundled default
//! (`assets/sounds/clipuh_05_synth_check.wav`, embedded in the executable)
//! or the user's imported sound.
//!
//! The imported sound is read once (on startup or when it is chosen) and
//! kept in memory, so saving a clip never touches the disk for its sound.
//! When the selected file is missing or unreadable the bundled sound plays
//! instead and Settings says why. Playback goes through
//! `clipper_platform::windows::sound` (asynchronous, rate limited); a sound
//! that cannot play never affects the save itself.

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use clipper_platform::windows::sound;
use clipper_storage::NotificationSettings;

/// The approved default clip-saved sound, unmodified.
const DEFAULT_WAV: &[u8] = include_bytes!("../../../assets/sounds/clipuh_05_synth_check.wav");
/// Display name of the bundled sound.
pub const DEFAULT_NAME: &str = "clipuh Synth Check";
/// Largest stored sound read back into memory (imports are at most about
/// 2 MB: 10 s of 16-bit 48 kHz stereo).
const MAX_STORED_BYTES: u64 = 8 * 1024 * 1024;

fn default_wav() -> Arc<[u8]> {
    static DEFAULT: OnceLock<Arc<[u8]>> = OnceLock::new();
    DEFAULT.get_or_init(|| Arc::from(DEFAULT_WAV)).clone()
}

/// The imported sound held in memory, by file name.
static CUSTOM: Mutex<Option<(String, Arc<[u8]>)>> = Mutex::new(None);

/// Which sound a toast plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoundKind {
    /// A clip was saved and finalised.
    Success,
    /// Saving failed: the short system error sound, never the success sound.
    Error,
}

/// Reads a stored sound (blocking: call off the UI thread).
pub fn read_stored(folder: &Path, name: &str) -> Result<Arc<[u8]>, String> {
    if !clipper_storage::is_plain_file_name(name) {
        return Err("The sound file name is invalid.".into());
    }
    let path = folder.join(name);
    let size = std::fs::metadata(&path)
        .map_err(|_| format!("The custom sound {name} is missing; using the default sound."))?
        .len();
    if size > MAX_STORED_BYTES {
        return Err(format!(
            "The custom sound {name} is too large; using the default sound."
        ));
    }
    let bytes = std::fs::read(&path).map_err(|error| {
        format!("The custom sound {name} could not be read ({error}); using the default sound.")
    })?;
    if bytes.len() <= 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(format!(
            "The custom sound {name} is damaged; using the default sound."
        ));
    }
    Ok(Arc::from(bytes))
}

/// Name shown for a stored sound: the file name without the content
/// fingerprint and extension (`ping-0123456789abcdef.wav` -> `ping`).
pub fn display_name(stored: &str) -> &str {
    let stem = stored.strip_suffix(".wav").unwrap_or(stored);
    match stem.rsplit_once('-') {
        Some((name, hash))
            if hash.len() == 16
                && hash.bytes().all(|b| b.is_ascii_hexdigit())
                && !name.is_empty() =>
        {
            name
        }
        _ => stem,
    }
}

/// Keeps `bytes` as the in-memory copy of the imported sound `name`.
pub fn set_loaded(name: &str, bytes: Arc<[u8]>) {
    *CUSTOM.lock().unwrap_or_else(PoisonError::into_inner) = Some((name.to_owned(), bytes));
}

/// Whether the imported sound `name` is in memory.
pub fn is_loaded(name: &str) -> bool {
    CUSTOM
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .is_some_and(|(loaded, _)| loaded == name)
}

/// The success sound for `settings`: the selected imported sound when it is
/// loaded, otherwise the bundled default.
fn success_wav(settings: &NotificationSettings) -> Arc<[u8]> {
    let Some(name) = settings.selected_custom_sound() else {
        return default_wav();
    };
    CUSTOM
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .filter(|(loaded, _)| loaded == name)
        .map_or_else(default_wav, |(_, bytes)| bytes.clone())
}

/// Plays `kind` if sounds are on. Never blocks; failures are only logged.
pub fn play(kind: SoundKind, settings: &NotificationSettings) {
    if !settings.sound {
        return;
    }
    match kind {
        SoundKind::Success => match sound::play_wav(success_wav(settings)) {
            Ok(()) | Err(sound::PlayError::RateLimited) => {}
            Err(error) => {
                clipper_diagnostics::log::warn!("notification sound unavailable: {error:?}")
            }
        },
        SoundKind::Error => sound::play_error(),
    }
}

/// Settings preview: the success sound exactly as a saved clip plays it.
pub fn preview(settings: &NotificationSettings) {
    let settings = NotificationSettings {
        sound: true,
        ..settings.clone()
    };
    play(SoundKind::Success, &settings);
}

#[cfg(test)]
mod tests {
    use super::*;
    use clipper_storage::SoundChoice;

    #[test]
    fn bundled_default_is_the_approved_wav() {
        assert_eq!(&DEFAULT_WAV[0..4], b"RIFF");
        assert_eq!(&DEFAULT_WAV[8..12], b"WAVE");
        // 0.72 s of 16-bit 48 kHz stereo plus header.
        assert_eq!(DEFAULT_WAV.len(), 138_284);
    }

    #[test]
    fn default_selection_and_missing_custom_sound_use_the_bundled_wav() {
        let mut settings = NotificationSettings::default();
        assert_eq!(&*success_wav(&settings), DEFAULT_WAV);
        settings.sound_choice = SoundChoice::Custom;
        settings.custom_sound = Some("never-loaded-0000.wav".into());
        assert_eq!(&*success_wav(&settings), DEFAULT_WAV, "falls back");
    }

    #[test]
    fn a_loaded_custom_sound_is_used_only_while_selected() {
        let custom: Arc<[u8]> =
            Arc::from(&b"RIFF....WAVEcustom-sound-bytes-for-test-only..........."[..]);
        set_loaded("test-custom-1.wav", custom.clone());
        let mut settings = NotificationSettings {
            sound_choice: SoundChoice::Custom,
            custom_sound: Some("test-custom-1.wav".into()),
            ..NotificationSettings::default()
        };
        assert!(is_loaded("test-custom-1.wav"));
        assert_eq!(&*success_wav(&settings), &*custom);
        settings.sound_choice = SoundChoice::Default;
        assert_eq!(&*success_wav(&settings), DEFAULT_WAV);
    }

    #[test]
    fn stored_sounds_are_validated_when_read() {
        let dir = std::env::temp_dir().join(format!("clipuh-sounds-read-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(
            read_stored(&dir, "missing.wav")
                .unwrap_err()
                .contains("missing")
        );
        std::fs::write(
            dir.join("junk.wav"),
            b"not a wav file at all, definitely not one....",
        )
        .unwrap();
        assert!(
            read_stored(&dir, "junk.wav")
                .unwrap_err()
                .contains("damaged")
        );
        std::fs::write(dir.join("ok.wav"), DEFAULT_WAV).unwrap();
        assert_eq!(
            read_stored(&dir, "ok.wav").unwrap().len(),
            DEFAULT_WAV.len()
        );
        assert!(read_stored(&dir, "../ok.wav").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn display_names_hide_the_fingerprint() {
        assert_eq!(display_name("ping-0123456789abcdef.wav"), "ping");
        assert_eq!(display_name("My Sound-0123456789abcdef.wav"), "My Sound");
        assert_eq!(display_name("odd-name.wav"), "odd-name");
        assert_eq!(display_name("x"), "x");
    }

    #[test]
    fn disabled_sounds_play_nothing() {
        // `play` returns before touching the player when sounds are off.
        let settings = NotificationSettings {
            sound: false,
            ..NotificationSettings::default()
        };
        play(SoundKind::Success, &settings);
        play(SoundKind::Error, &settings);
    }
}

#[cfg(test)]
mod device_tests {
    use super::*;

    #[test]
    #[ignore = "plays the bundled sound on the default output device"]
    fn bundled_sound_is_accepted_by_the_system_player() {
        assert_eq!(sound::play_wav(default_wav()), Ok(()));
        std::thread::sleep(std::time::Duration::from_millis(900));
        sound::stop();
    }
}
