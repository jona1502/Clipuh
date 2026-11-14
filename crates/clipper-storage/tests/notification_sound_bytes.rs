//! Storing a WAV produced in memory (e.g. a decoded MP3) uses the same
//! validation and atomic, collision-safe publishing as a file import.

use std::path::Path;

use clipper_storage::store_notification_wav;

fn stereo_48k_wav(frames: u32) -> Vec<u8> {
    let data = frames * 4;
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&48_000u32.to_le_bytes());
    out.extend_from_slice(&(48_000u32 * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for i in 0..frames {
        let v = ((i as f32 * 0.05).sin() * 8000.) as i16;
        out.extend_from_slice(&v.to_le_bytes());
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("clipuh-sound-bytes-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn decoded_wav_is_validated_and_stored_without_overwriting() {
    let dir = temp_dir("ok");
    let wav = stereo_48k_wav(48_000);
    let first = store_notification_wav(Path::new("Ping Ünïcode.mp3"), &wav, &dir).unwrap();
    let second = store_notification_wav(Path::new("Ping Ünïcode.mp3"), &wav, &dir).unwrap();
    assert_ne!(first, second, "a second import never replaces the first");
    assert_eq!(std::fs::read(&first).unwrap(), wav);
    assert_eq!(first.extension().unwrap(), "wav");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn invalid_or_oversized_images_store_nothing() {
    let dir = temp_dir("bad");
    assert!(store_notification_wav(Path::new("x.mp3"), b"not a wav", &dir).is_err());
    let huge = vec![0u8; 6 * 1024 * 1024];
    assert!(store_notification_wav(Path::new("x.mp3"), &huge, &dir).is_err());
    assert!(!dir.exists() || std::fs::read_dir(&dir).unwrap().next().is_none());
    let _ = std::fs::remove_dir_all(dir);
}
