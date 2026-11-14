//! Importing a user's notification sound.
//!
//! The chosen file is fully decoded with Media Foundation (the decoder the
//! editor already uses), checked against size, length and silence limits,
//! and written as a canonical 16-bit 48 kHz stereo WAV into clipuh's sound
//! folder. Playback then only ever handles that WAV, so a sound that imported
//! fine always plays, and the original file can move or disappear.
//!
//! Supported: WAV and MP3 (both decode with Media Foundation on every
//! Windows 10/11 install). OGG is not offered: Windows has no built-in
//! Vorbis decoder.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// File extensions the import accepts (lowercase).
pub const SUPPORTED_EXTENSIONS: [&str; 2] = ["wav", "mp3"];
/// Largest source file read.
pub const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
/// Longest notification sound.
pub const MAX_DURATION: Duration = Duration::from_secs(10);
/// Peaks are brought down to -1 dBFS (never amplified), so loud files do
/// not clip on playback.
pub const PEAK_CEILING: f32 = 0.891;
/// Anything quieter than about -60 dBFS everywhere counts as silent.
const SILENCE: f32 = 0.001;
const RATE: u32 = 48_000;
const CHANNELS: u16 = 2;

#[derive(Debug, PartialEq, Eq)]
pub enum ImportError {
    Unsupported,
    TooLarge,
    TooLong,
    Silent,
    /// The file could not be decoded (corrupt or not really audio).
    Decode(String),
    /// Reading the source or writing the sound folder failed.
    Io(String),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => f.write_str("Choose a WAV or MP3 file."),
            Self::TooLarge => f.write_str("The file is larger than 50 MB."),
            Self::TooLong => f.write_str("Notification sounds can be at most 10 seconds long."),
            Self::Silent => f.write_str("The sound is silent."),
            Self::Decode(error) => write!(f, "The file could not be read as audio: {error}"),
            Self::Io(error) => write!(f, "The sound could not be saved: {error}"),
        }
    }
}

impl std::error::Error for ImportError {}

/// `true` for a file name with a supported extension.
pub fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            SUPPORTED_EXTENSIONS
                .iter()
                .any(|supported| extension.eq_ignore_ascii_case(supported))
        })
}

/// Decodes `path` into a canonical WAV image. Blocking; run off the UI thread.
pub fn decode_to_wav(path: &Path) -> Result<Vec<u8>, ImportError> {
    if !is_supported(path) {
        return Err(ImportError::Unsupported);
    }
    let size = std::fs::metadata(path)
        .map_err(|error| ImportError::Io(error.to_string()))?
        .len();
    if size > MAX_FILE_BYTES {
        return Err(ImportError::TooLarge);
    }
    let tracks = crate::audio::discover(path).map_err(ImportError::Decode)?;
    let stream = tracks
        .first()
        .ok_or_else(|| ImportError::Decode("no audio stream".into()))?
        .stream_index;
    let mut decoder = crate::audio::Decoder::open(path, &[stream]).map_err(ImportError::Decode)?;
    let cancel = AtomicBool::new(false);
    let limit = (MAX_DURATION.as_secs_f64() * f64::from(RATE)) as usize * CHANNELS as usize;
    let mut samples: Vec<f32> = Vec::new();
    while let Some(chunk) = decoder.next(&cancel).map_err(ImportError::Decode)? {
        samples.extend_from_slice(&chunk.samples);
        if samples.len() > limit {
            return Err(ImportError::TooLong);
        }
    }
    finish(samples)
}

/// Limits and normalises decoded stereo 48 kHz samples, then encodes them.
pub fn finish(mut samples: Vec<f32>) -> Result<Vec<u8>, ImportError> {
    samples.retain(|sample| sample.is_finite());
    if samples.len() < CHANNELS as usize {
        return Err(ImportError::Decode("no audio data".into()));
    }
    let peak = samples.iter().fold(0f32, |peak, s| peak.max(s.abs()));
    if peak < SILENCE {
        return Err(ImportError::Silent);
    }
    if peak > PEAK_CEILING {
        let gain = PEAK_CEILING / peak;
        for sample in &mut samples {
            *sample *= gain;
        }
    }
    Ok(encode_wav(&samples))
}

/// 16-bit PCM stereo 48 kHz WAV image of interleaved `samples`.
pub fn encode_wav(samples: &[f32]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&CHANNELS.to_le_bytes());
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * u32::from(CHANNELS) * 2).to_le_bytes());
    out.extend_from_slice(&(CHANNELS * 2).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        let value = (sample.clamp(-1., 1.) * f32::from(i16::MAX)).round() as i16;
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

/// FNV-1a 64: a stable content fingerprint for de-duplicating imports.
pub fn fingerprint(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// File name for an imported sound: a cleaned-up stem of the source plus the
/// content fingerprint, so the same sound maps to the same file and
/// different sounds never overwrite each other.
pub fn stored_name(source: &Path, wav: &[u8]) -> String {
    let stem = source
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let clean: String = stem
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim()
        .chars()
        .take(48)
        .collect();
    let stem = if clean.is_empty() {
        "sound".into()
    } else {
        clean
    };
    format!("{stem}-{:016x}.wav", fingerprint(wav))
}

/// Writes `wav` as `name` into `folder` atomically (temp file + rename).
/// An existing file with the same name has the same content (fingerprinted
/// name) and is kept as is. Returns the stored file's path.
pub fn store(folder: &Path, name: &str, wav: &[u8]) -> Result<PathBuf, ImportError> {
    let io = |error: std::io::Error| ImportError::Io(error.to_string());
    std::fs::create_dir_all(folder).map_err(io)?;
    let target = folder.join(name);
    if target.is_file() {
        return Ok(target);
    }
    let partial = folder.join(format!("{name}.partial"));
    let written = (|| {
        let mut file = std::fs::File::create(&partial)?;
        file.write_all(wav)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&partial, &target)
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&partial);
        return Err(io(error));
    }
    Ok(target)
}

/// Full import: decode, limit, store. Returns the stored file name (relative
/// to `folder`). Blocking. The source file is only read.
pub fn import(source: &Path, folder: &Path) -> Result<String, ImportError> {
    let wav = decode_to_wav(source)?;
    let name = stored_name(source, &wav);
    store(folder, &name, &wav)?;
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "clipuh-sound-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn tone(seconds: f32, amplitude: f32) -> Vec<f32> {
        let frames = (seconds * RATE as f32) as usize;
        (0..frames)
            .flat_map(|i| {
                let v = (i as f32 * 660. * std::f32::consts::TAU / RATE as f32).sin() * amplitude;
                [v, v]
            })
            .collect()
    }

    #[test]
    fn only_wav_and_mp3_are_offered() {
        assert!(is_supported(Path::new("a.wav")));
        assert!(is_supported(Path::new("B.MP3")));
        for name in ["a.ogg", "a.flac", "a", "a.wav.txt", "a.exe"] {
            assert!(!is_supported(Path::new(name)), "{name}");
        }
        let dir = temp_dir("ext");
        let ogg = dir.join("x.ogg");
        std::fs::write(&ogg, b"OggS").unwrap();
        assert_eq!(decode_to_wav(&ogg), Err(ImportError::Unsupported));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn finish_rejects_silence_and_limits_loud_peaks_without_amplifying() {
        assert_eq!(finish(vec![0.; 4800]), Err(ImportError::Silent));
        assert_eq!(
            finish(Vec::new()).unwrap_err(),
            ImportError::Decode("no audio data".into())
        );
        let quiet = finish(tone(0.1, 0.2)).unwrap();
        let loud = finish(tone(0.1, 1.5)).unwrap();
        let peak = |wav: &[u8]| {
            wav[44..]
                .chunks(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs())
                .max()
                .unwrap()
        };
        let ceiling = (PEAK_CEILING * f32::from(i16::MAX)) as u16;
        assert!(peak(&loud) <= ceiling + 1, "loud file is limited");
        assert!(
            peak(&quiet) < (0.21 * f32::from(i16::MAX)) as u16,
            "quiet stays quiet"
        );
    }

    #[test]
    fn wav_header_describes_the_data() {
        let wav = encode_wav(&[0.5, -0.5, 1.0, -1.0]);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), RATE);
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 8);
        assert_eq!(wav.len(), 44 + 8);
    }

    #[test]
    fn decodes_a_real_wav_file_through_media_foundation() {
        let dir = temp_dir("decode");
        let source = dir.join("Signal Ünïcode 片段.wav");
        std::fs::write(&source, encode_wav(&tone(0.5, 0.5))).unwrap();
        let wav = decode_to_wav(&source).expect("decodes");
        // About half a second of stereo 48 kHz 16-bit audio.
        let seconds = (wav.len() - 44) as f32 / (RATE as f32 * 4.);
        assert!((seconds - 0.5).abs() < 0.05, "{seconds}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupt_and_too_long_files_are_rejected() {
        let dir = temp_dir("bad");
        let corrupt = dir.join("broken.wav");
        std::fs::write(&corrupt, b"RIFF\x10\x00\x00\x00WAVEjunkjunkjunkjunk").unwrap();
        assert!(matches!(
            decode_to_wav(&corrupt),
            Err(ImportError::Decode(_))
        ));
        let fake_mp3 = dir.join("fake.mp3");
        std::fs::write(&fake_mp3, vec![0x55u8; 4096]).unwrap();
        assert!(matches!(
            decode_to_wav(&fake_mp3),
            Err(ImportError::Decode(_))
        ));
        let long = dir.join("long.wav");
        std::fs::write(&long, encode_wav(&tone(10.5, 0.3))).unwrap();
        assert_eq!(decode_to_wav(&long), Err(ImportError::TooLong));
        assert!(matches!(
            decode_to_wav(&dir.join("missing.wav")),
            Err(ImportError::Io(_))
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn import_stores_once_keeps_the_source_and_never_overwrites_others() {
        let dir = temp_dir("import");
        let folder = dir.join("sounds");
        let a = dir.join("ping.wav");
        let b = dir.join("other ping.wav");
        std::fs::write(&a, encode_wav(&tone(0.3, 0.4))).unwrap();
        std::fs::write(&b, encode_wav(&tone(0.4, 0.4))).unwrap();
        let first = import(&a, &folder).unwrap();
        let again = import(&a, &folder).unwrap();
        assert_eq!(first, again, "same sound, same file");
        let other = import(&b, &folder).unwrap();
        assert_ne!(first, other);
        let stored: Vec<_> = std::fs::read_dir(&folder).unwrap().collect();
        assert_eq!(stored.len(), 2, "no duplicates, no partial files");
        assert!(a.is_file() && b.is_file(), "sources untouched");
        assert!(first.starts_with("ping-") && first.ends_with(".wav"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn failed_store_leaves_no_partial_file() {
        let dir = temp_dir("readonly");
        // A file where the folder should be: creating the folder fails.
        let blocked = dir.join("sounds");
        std::fs::write(&blocked, b"not a folder").unwrap();
        assert!(matches!(
            store(&blocked, "x.wav", &encode_wav(&[0.1, 0.1])),
            Err(ImportError::Io(_))
        ));
        assert!(std::fs::read_dir(&dir).unwrap().count() == 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn stored_names_are_clean_and_content_addressed() {
        let wav = encode_wav(&[0.1, 0.1]);
        let name = stored_name(Path::new(r"C:\x\My Sound: <1>.wav"), &wav);
        assert!(name.starts_with("My Sound_ _1_-"), "{name}");
        assert_eq!(name, stored_name(Path::new("My Sound: <1>.mp3"), &wav));
        assert!(stored_name(Path::new(""), &wav).starts_with("sound-"));
        assert_ne!(fingerprint(b"a"), fingerprint(b"b"));
    }
}
