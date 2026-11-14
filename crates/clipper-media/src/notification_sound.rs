//! Decoding a notification sound the user picked into a WAV image.
//!
//! clipper-storage (`import_notification_wav_bytes`) validates and stores
//! WAV files; this module turns formats Windows can decode with Media
//! Foundation (the decoder the editor already uses) into a 16-bit 48 kHz
//! stereo WAV first, so MP3 files can be imported too. The result is limited
//! to 10 seconds, rejected when silent, and its peaks are brought down to
//! -1 dBFS (never amplified).
//!
//! Supported: WAV and MP3 (both decode with Media Foundation on every
//! Windows 10/11 install). OGG is not offered: Windows has no built-in
//! Vorbis decoder.

use std::path::Path;
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
    /// Reading the source file failed.
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
            Self::Io(error) => write!(f, "The file could not be read: {error}"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
}
