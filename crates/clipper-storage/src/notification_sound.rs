//! Validated, application-owned copies of user-selected notification sounds.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX_NOTIFICATION_SOUND_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_NOTIFICATION_SOUND_SECONDS: u32 = 10;
static NEXT_IMPORT: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub enum NotificationSoundError {
    Invalid(&'static str),
    Io(io::Error),
    Config(crate::ConfigError),
}

impl std::fmt::Display for NotificationSoundError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => f.write_str(message),
            Self::Io(error) => write!(f, "Cannot import notification sound: {error}"),
            Self::Config(error) => write!(f, "Cannot locate notification sound storage: {error}"),
        }
    }
}

impl std::error::Error for NotificationSoundError {}

impl From<io::Error> for NotificationSoundError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Only uncompressed integer PCM understood by the native Windows player.
/// Chunk sizes, padding, format arithmetic and duration are validated before
/// any managed file is created. No decoder or external process is involved.
pub fn validate_notification_wav(bytes: &[u8]) -> Result<(), NotificationSoundError> {
    use NotificationSoundError::Invalid;
    if bytes.len() > MAX_NOTIFICATION_SOUND_BYTES {
        return Err(Invalid("Choose a WAV file no larger than 5 MiB."));
    }
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(Invalid("Choose a RIFF/WAVE audio file."));
    }
    let u32_at = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
    if u64::from(u32_at(4)) + 8 != bytes.len() as u64 {
        return Err(Invalid(
            "The WAV file is truncated or has an invalid RIFF length.",
        ));
    }
    let mut position = 12usize;
    let mut format = None;
    let mut data = None;
    while position < bytes.len() {
        let header_end = position
            .checked_add(8)
            .filter(|end| *end <= bytes.len())
            .ok_or(Invalid("The WAV file contains a truncated chunk header."))?;
        let length = usize::try_from(u32_at(position + 4))
            .map_err(|_| Invalid("Invalid WAV chunk length."))?;
        let end = header_end
            .checked_add(length)
            .filter(|end| *end <= bytes.len())
            .ok_or(Invalid("The WAV file contains a truncated chunk."))?;
        match &bytes[position..position + 4] {
            b"fmt " => {
                if format.is_some() || data.is_some() || ![16, 18].contains(&length) {
                    return Err(Invalid(
                        "The WAV file has an unsupported or duplicate format chunk.",
                    ));
                }
                let field = |offset| {
                    u16::from_le_bytes(
                        bytes[header_end + offset..header_end + offset + 2]
                            .try_into()
                            .unwrap(),
                    )
                };
                let (encoding, channels, rate, byte_rate, alignment, bits) = (
                    field(0),
                    field(2),
                    u32_at(header_end + 4),
                    u32_at(header_end + 8),
                    field(12),
                    field(14),
                );
                if encoding != 1
                    || ![1, 2].contains(&channels)
                    || ![8, 16].contains(&bits)
                    || !(8000..=192000).contains(&rate)
                    || (length == 18 && field(16) != 0)
                {
                    return Err(Invalid(
                        "Use PCM WAV: 8-bit or 16-bit, mono or stereo, 8–192 kHz.",
                    ));
                }
                let expected_alignment = channels * (bits / 8);
                if alignment != expected_alignment || byte_rate != rate * u32::from(alignment) {
                    return Err(Invalid(
                        "The WAV file has inconsistent sample alignment or byte rate.",
                    ));
                }
                format = Some((rate, alignment));
            }
            b"data" => {
                if format.is_none() || data.is_some() {
                    return Err(Invalid(
                        "The WAV file needs one format chunk followed by one audio chunk.",
                    ));
                }
                data = Some(length);
            }
            _ => {}
        }
        position = end
            .checked_add(length % 2)
            .filter(|end| *end <= bytes.len())
            .ok_or(Invalid("The WAV file is missing chunk padding."))?;
    }
    let (rate, alignment) = format.ok_or(Invalid("The WAV file has no PCM format chunk."))?;
    let length = data.ok_or(Invalid("The WAV file has no audio data."))?;
    if length == 0 || length % usize::from(alignment) != 0 {
        return Err(Invalid(
            "The WAV file contains empty or incomplete audio frames.",
        ));
    }
    if length as u64
        > u64::from(rate) * u64::from(alignment) * u64::from(MAX_NOTIFICATION_SOUND_SECONDS)
    {
        return Err(Invalid(
            "Choose a notification sound no longer than 10 seconds.",
        ));
    }
    Ok(())
}

fn unique_suffix() -> String {
    format!(
        "{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        NEXT_IMPORT.fetch_add(1, Ordering::Relaxed)
    )
}

/// Run on a background worker: validation and copying perform filesystem I/O.
pub fn import_notification_sound(source: &Path) -> Result<PathBuf, NotificationSoundError> {
    let config = crate::config_path().map_err(NotificationSoundError::Config)?;
    let parent = config.parent().ok_or(NotificationSoundError::Invalid(
        "Cannot locate notification sound storage.",
    ))?;
    import_notification_sound_to(source, &parent.join("sounds"))
}

/// Import to a caller-selected managed directory. Copies the exact validated
/// bytes, so later changes or removal of the original cannot affect playback.
/// Existing imported sounds remain untouched if validation or writing fails.
pub fn import_notification_sound_to(
    source: &Path,
    directory: &Path,
) -> Result<PathBuf, NotificationSoundError> {
    let file = File::open(source)?;
    if !file.metadata()?.is_file() {
        return Err(NotificationSoundError::Invalid(
            "Choose a regular WAV audio file.",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_NOTIFICATION_SOUND_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    store_notification_wav(source, &bytes, directory)
}

/// Like [`import_notification_sound`] for a WAV image produced in memory
/// (e.g. an MP3 decoded to WAV). `source` only names the stored file.
pub fn import_notification_wav_bytes(
    source: &Path,
    bytes: &[u8],
) -> Result<PathBuf, NotificationSoundError> {
    let config = crate::config_path().map_err(NotificationSoundError::Config)?;
    let parent = config.parent().ok_or(NotificationSoundError::Invalid(
        "Cannot locate notification sound storage.",
    ))?;
    store_notification_wav(source, bytes, &parent.join("sounds"))
}

/// Validates `bytes` and publishes them as a new managed WAV in `directory`.
pub fn store_notification_wav(
    source: &Path,
    bytes: &[u8],
    directory: &Path,
) -> Result<PathBuf, NotificationSoundError> {
    if bytes.len() > MAX_NOTIFICATION_SOUND_BYTES {
        return Err(NotificationSoundError::Invalid(
            "Notification sounds can be at most 5 MiB.",
        ));
    }
    validate_notification_wav(bytes)?;
    fs::create_dir_all(directory)?;
    let stem: String = source
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .chars()
        .take(48)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let stem = if stem.trim_matches('-').is_empty() {
        "custom"
    } else {
        &stem
    };
    let name = format!("{stem}-{}", unique_suffix());
    let temporary = directory.join(format!(".{name}.tmp"));
    let destination = directory.join(format!("{name}.wav"));
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| {
        output.write_all(bytes)?;
        output.sync_all()?;
        drop(output);
        // Linking publishes the complete file atomically and refuses to
        // overwrite any existing name, even if a second importer races us.
        fs::hard_link(&temporary, &destination)?;
        Ok::<_, io::Error>(destination)
    })();
    let _ = fs::remove_file(&temporary);
    result.map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(channels: u16, rate: u32, bits: u16, frames: u32) -> Vec<u8> {
        let alignment = channels * bits / 8;
        let bytes = frames * u32::from(alignment);
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&(36 + bytes + bytes % 2).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * u32::from(alignment)).to_le_bytes());
        out.extend_from_slice(&alignment.to_le_bytes());
        out.extend_from_slice(&bits.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&bytes.to_le_bytes());
        out.resize(44 + bytes as usize + bytes as usize % 2, 0);
        out
    }

    #[test]
    fn accepts_supported_pcm_and_exact_duration_limit() {
        for channels in [1, 2] {
            for bits in [8, 16] {
                assert!(validate_notification_wav(&wav(channels, 8000, bits, 80000)).is_ok());
                assert!(validate_notification_wav(&wav(channels, 48000, bits, 1)).is_ok());
            }
        }
        assert!(validate_notification_wav(&wav(2, 192000, 16, 1)).is_ok());
        assert!(validate_notification_wav(&wav(1, 8000, 8, 80001)).is_err());
    }

    #[test]
    fn rejects_malformed_unsupported_and_truncated_chunks() {
        let valid = wav(2, 48000, 16, 20);
        for length in 0..valid.len() {
            assert!(
                validate_notification_wav(&valid[..length]).is_err(),
                "length {length}"
            );
        }
        for (position, replacement) in [(0, 0), (8, 0), (20, 3), (22, 6), (34, 24)] {
            let mut invalid = valid.clone();
            invalid[position] = replacement;
            assert!(
                validate_notification_wav(&invalid).is_err(),
                "position {position}"
            );
        }
        for position in [4, 16, 40] {
            let mut invalid = valid.clone();
            invalid[position..position + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            assert!(validate_notification_wav(&invalid).is_err());
        }
        assert!(validate_notification_wav(&wav(1, 7999, 8, 20)).is_err());
        assert!(validate_notification_wav(&wav(1, 192001, 8, 20)).is_err());
        assert!(validate_notification_wav(&wav(1, 8000, 8, 0)).is_err());
        assert!(validate_notification_wav(&vec![0; MAX_NOTIFICATION_SOUND_BYTES + 1]).is_err());
    }

    #[test]
    fn chunk_padding_duplicates_and_frame_alignment_are_checked() {
        let valid = wav(2, 48000, 16, 20);
        let resize_riff = |bytes: &mut Vec<u8>| {
            let length = bytes.len() as u32 - 8;
            bytes[4..8].copy_from_slice(&length.to_le_bytes());
        };
        let mut padded = valid.clone();
        padded.splice(12..12, [b'J', b'U', b'N', b'K', 1, 0, 0, 0, 0xAA, 0]);
        resize_riff(&mut padded);
        assert!(validate_notification_wav(&padded).is_ok());
        padded.remove(21);
        resize_riff(&mut padded);
        assert!(validate_notification_wav(&padded).is_err());
        let mut duplicate_format = valid.clone();
        duplicate_format.splice(36..36, valid[12..36].iter().copied());
        resize_riff(&mut duplicate_format);
        assert!(validate_notification_wav(&duplicate_format).is_err());
        let mut duplicate_data = valid.clone();
        duplicate_data.extend_from_slice(&valid[36..]);
        resize_riff(&mut duplicate_data);
        assert!(validate_notification_wav(&duplicate_data).is_err());
        let mut incomplete = wav(1, 8000, 16, 1);
        incomplete[40..44].copy_from_slice(&1u32.to_le_bytes());
        assert!(validate_notification_wav(&incomplete).is_err());
    }

    #[test]
    fn import_copies_validated_bytes_and_preserves_prior_imports() {
        let directory = std::env::temp_dir().join(format!(
            "clipper-sound-test-{}-{}",
            std::process::id(),
            unique_suffix()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let source = directory.join("My chime.wav");
        let managed = directory.join("managed");
        let bytes = wav(1, 48000, 16, 480);
        std::fs::write(&source, &bytes).unwrap();
        let first = import_notification_sound_to(&source, &managed).unwrap();
        let second = import_notification_sound_to(&source, &managed).unwrap();
        assert_ne!(first, second);
        assert!(
            first
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("My-chime-")
        );
        std::fs::remove_file(&source).unwrap();
        assert_eq!(std::fs::read(&first).unwrap(), bytes);
        assert_eq!(std::fs::read(&second).unwrap(), bytes);
        let invalid = directory.join("bad.wav");
        std::fs::write(&invalid, b"bad").unwrap();
        assert!(import_notification_sound_to(&invalid, &managed).is_err());
        assert_eq!(std::fs::read_dir(&managed).unwrap().count(), 2);
        // Every path belongs to this test's uniquely created temporary directory.
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
