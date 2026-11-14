//! Names the audio tracks of a finished MP4 written by another muxer (Media
//! Foundation's sink writer for manual recordings). Each audio `trak` gets a
//! `udta/name` box, which Media Foundation reports as the stream name, so the
//! editor shows the recorded source instead of guessing from stream order.
//!
//! Crash-safe in place: the updated `moov` is appended after the old one and
//! only then is the old one renamed to `free` (a 4-byte write). Every
//! intermediate state is a playable file. Only files whose `moov` is the last
//! top-level box are touched (the sink writer's default layout), so no chunk
//! offset ever moves.

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::error::ReplayError;
use crate::mp4::handler_name;

/// Larger `moov` boxes are left alone (a recording's is a few hundred KiB).
const MAX_MOOV: u64 = 64 << 20;

/// `(type, start, header length, total length)` of one box.
type BoxHeader = ([u8; 4], u64, u64, u64);

fn header_at(data: &[u8], at: usize) -> Option<BoxHeader> {
    let size = u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?);
    let kind: [u8; 4] = data.get(at + 4..at + 8)?.try_into().ok()?;
    let (header, total) = match size {
        1 => (
            16,
            u64::from_be_bytes(data.get(at + 8..at + 16)?.try_into().ok()?),
        ),
        0 => (8, (data.len() - at) as u64),
        size => (8, u64::from(size)),
    };
    (total >= header && at as u64 + total <= data.len() as u64)
        .then_some((kind, at as u64, header, total))
}

/// Child boxes of `data[range]`.
fn children(data: &[u8], start: usize, end: usize) -> Option<Vec<BoxHeader>> {
    let mut out = Vec::new();
    let mut at = start;
    while at < end {
        let header = header_at(&data[..end], at)?;
        at += header.3 as usize;
        out.push(header);
    }
    Some(out)
}

fn find(data: &[u8], parent: &BoxHeader, kind: &[u8; 4]) -> Option<BoxHeader> {
    let start = (parent.1 + parent.2) as usize;
    let end = (parent.1 + parent.3) as usize;
    children(data, start, end)?
        .into_iter()
        .find(|b| &b.0 == kind)
}

fn make_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(&(body.len() as u32 + 8).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out
}

/// `moov` with `udta/name` added to each audio `trak`, or `None` when the
/// track count does not match `labels`, a track is already named, or the box
/// tree is not understood. `moov` is the whole box including its header.
fn named_moov(moov: &[u8], labels: &[String]) -> Option<Vec<u8>> {
    let root = header_at(moov, 0)?;
    let tracks = children(moov, root.2 as usize, moov.len())?;
    let audio: Vec<&BoxHeader> = tracks
        .iter()
        .filter(|b| &b.0 == b"trak")
        .filter(|trak| {
            find(moov, trak, b"mdia")
                .and_then(|mdia| find(moov, &mdia, b"hdlr"))
                // hdlr: version/flags (4), pre_defined (4), handler_type (4).
                .and_then(|hdlr| moov.get((hdlr.1 + hdlr.2 + 8) as usize..)?.get(..4))
                == Some(b"soun".as_slice())
        })
        .collect();
    if audio.len() != labels.len() || audio.iter().any(|t| find(moov, t, b"udta").is_some()) {
        return None;
    }
    let mut body = Vec::with_capacity(moov.len() + labels.len() * 64);
    let mut next_label = labels.iter();
    for child in &tracks {
        let bytes = &moov[child.1 as usize..(child.1 + child.3) as usize];
        if audio.iter().any(|a| a.1 == child.1) {
            let label = next_label.next()?;
            let udta = make_box(
                b"udta",
                &make_box(b"name", handler_name(label, false).as_bytes()),
            );
            let mut trak = bytes[child.2 as usize..].to_vec();
            trak.extend_from_slice(&udta);
            body.extend_from_slice(&make_box(b"trak", &trak));
        } else {
            body.extend_from_slice(bytes);
        }
    }
    Some(make_box(b"moov", &body))
}

/// Adds source names to the audio tracks of `path` in place. `Ok(false)`
/// means the file was left untouched (layout not supported, names already
/// present, or `labels` does not match the audio track count).
pub fn name_audio_tracks(path: &Path, labels: &[String]) -> Result<bool, ReplayError> {
    let io = |what: &'static str| move |error| ReplayError::io(what, path, error);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(io("opening the recording"))?;
    let len = file.metadata().map_err(io("reading the recording"))?.len();
    // Walk top-level box headers only; mdat is never read.
    let mut at = 0u64;
    let mut moov = None;
    while at < len {
        let mut head = [0u8; 16];
        file.seek(SeekFrom::Start(at))
            .map_err(io("reading the recording"))?;
        let got = file.read(&mut head).map_err(io("reading the recording"))?;
        let size = u64::from(u32::from_be_bytes(head[..4].try_into().unwrap_or_default()));
        let total = match size {
            1 if got >= 16 => u64::from_be_bytes(head[8..16].try_into().unwrap_or_default()),
            0 => len - at,
            size => size,
        };
        if got < 8 || total < 8 || at + total > len {
            return Ok(false);
        }
        if &head[4..8] == b"moov" {
            moov = Some((at, total));
        }
        at += total;
    }
    let Some((start, total)) = moov.filter(|(start, total)| start + total == len) else {
        return Ok(false);
    };
    if total > MAX_MOOV {
        return Ok(false);
    }
    let mut old = vec![0u8; total as usize];
    file.seek(SeekFrom::Start(start))
        .map_err(io("reading the recording"))?;
    file.read_exact(&mut old)
        .map_err(io("reading the recording"))?;
    let Some(new) = named_moov(&old, labels) else {
        return Ok(false);
    };
    // 1. Append the named moov (the old one still comes first and wins).
    file.seek(SeekFrom::Start(len))
        .map_err(io("writing the recording"))?;
    file.write_all(&new).map_err(io("writing the recording"))?;
    file.sync_all().map_err(io("writing the recording"))?;
    // 2. Retire the old one: a single 4-byte type change.
    file.seek(SeekFrom::Start(start + 4))
        .map_err(io("writing the recording"))?;
    file.write_all(b"free")
        .map_err(io("writing the recording"))?;
    file.sync_all().map_err(io("writing the recording"))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trak(handler: &[u8; 4]) -> Vec<u8> {
        let mut hdlr = vec![0u8; 8];
        hdlr.extend_from_slice(handler);
        hdlr.extend_from_slice(&[0; 12]);
        hdlr.extend_from_slice(b"\0");
        let mdia = make_box(b"mdia", &make_box(b"hdlr", &hdlr));
        make_box(b"trak", &concat(&[make_box(b"tkhd", &[0; 84]), mdia]))
    }

    fn concat(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }

    /// ftyp, mdat, moov(mvhd, video, audio, audio): the sink writer layout.
    fn file(audio: usize) -> Vec<u8> {
        let mut traks = vec![make_box(b"mvhd", &[0; 100]), trak(b"vide")];
        traks.extend((0..audio).map(|_| trak(b"soun")));
        concat(&[
            make_box(b"ftyp", b"isom\0\0\0\0"),
            make_box(b"mdat", &[7; 64]),
            make_box(b"moov", &concat(&traks)),
        ])
    }

    fn names(data: &[u8]) -> Vec<String> {
        let top = children(data, 0, data.len()).unwrap();
        let moov = top.iter().find(|b| &b.0 == b"moov").unwrap();
        children(data, (moov.1 + moov.2) as usize, (moov.1 + moov.3) as usize)
            .unwrap()
            .iter()
            .filter(|b| &b.0 == b"trak")
            .filter_map(|t| find(data, t, b"udta"))
            .filter_map(|udta| find(data, &udta, b"name"))
            .map(|name| {
                String::from_utf8_lossy(
                    &data[(name.1 + name.2) as usize..(name.1 + name.3) as usize],
                )
                .into_owned()
            })
            .collect()
    }

    fn temp(name: &str, data: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "clipper-names-{name}-{}-{:?}.mp4",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&path, data).unwrap();
        path
    }

    fn labels(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn names_audio_tracks_in_order_and_keeps_media() {
        let original = file(3);
        let path = temp("ok", &original);
        let wanted = labels(&["Mix", "Discord.exe", "Microphone"]);
        assert!(name_audio_tracks(&path, &wanted).unwrap());
        let data = std::fs::read(&path).unwrap();
        assert_eq!(names(&data), wanted);
        // ftyp + mdat bytes are untouched, the old moov is now `free`.
        let top = children(&original, 0, original.len()).unwrap();
        let moov_at = top.last().unwrap().1 as usize;
        assert_eq!(data[..moov_at], original[..moov_at]);
        assert_eq!(&data[moov_at + 4..moov_at + 8], b"free");
        // Running again does nothing: tracks are already named.
        assert!(!name_audio_tracks(&path, &wanted).unwrap());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn mismatched_labels_or_layouts_leave_the_file_alone() {
        for (name, data, wanted) in [
            ("count", file(2), labels(&["Mix"])),
            ("none", file(0), labels(&["Mix"])),
            (
                "moov-first",
                concat(&[
                    make_box(b"ftyp", b"isom\0\0\0\0"),
                    make_box(b"moov", &trak(b"soun")),
                    make_box(b"mdat", &[7; 64]),
                ]),
                labels(&["Mix"]),
            ),
            ("garbage", vec![0, 0, 0, 3, b'x'], labels(&["Mix"])),
        ] {
            let path = temp(name, &data);
            assert!(!name_audio_tracks(&path, &wanted).unwrap(), "{name}");
            assert_eq!(std::fs::read(&path).unwrap(), data, "{name}");
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn single_mixed_track_is_named_mix() {
        let path = temp("single", &file(1));
        assert!(name_audio_tracks(&path, &labels(&["Mix"])).unwrap());
        assert_eq!(names(&std::fs::read(&path).unwrap()), ["Mix"]);
        let _ = std::fs::remove_file(path);
    }
}
