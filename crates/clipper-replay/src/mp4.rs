//! Minimal MP4 (ISO BMFF) muxer for H.264 + AAC, written in pure Rust.
//!
//! Layout: `ftyp`, `mdat` (samples interleaved in ~0.5 s per-track chunks,
//! streamed), `moov` at the end (no faststart). Video media timescale is
//! 90 000, audio uses its sample rate (AAC frames are exactly 1024 units),
//! the movie timescale is 1000. Every timestamp is converted from 100 ns
//! ticks individually with rounding (no cumulative drift), so the sum of
//! `stts` deltas always equals the converted track length.
//!
//! Edit lists carry the clip rebase: `media_time` skips the part of a media
//! timeline that precedes presentation time 0 (B-frame delay, audio packet
//! straddling the start) and an optional empty edit delays a late-starting
//! audio track. Composition offsets are never negative (`ctts` version 0).
//!
//! All audio tracks share `alternate_group` 1; only the first has the
//! in-movie flag set so players default to the primary mix.

use std::io::Write;
use std::path::Path;

use crate::error::ReplayError;
use crate::packet::{CodecConfig, StreamLayout, TICKS_PER_SECOND, Ticks};
use crate::plan::{ClipPlan, TrackPlan};

pub const VIDEO_TIMESCALE: u32 = 90_000;
const MOVIE_TIMESCALE: u32 = 1000;
/// Target chunk length (ticks of the shared timeline).
const CHUNK_TICKS: Ticks = TICKS_PER_SECOND / 2;

fn invalid(detail: impl Into<String>) -> ReplayError {
    ReplayError::Invalid(detail.into())
}

/// Rounds `ticks` (100 ns) to `timescale` units, half up.
fn convert(ticks: Ticks, timescale: u32) -> i64 {
    let scaled = i128::from(ticks) * i128::from(timescale);
    let unit = i128::from(TICKS_PER_SECOND);
    i64::try_from((scaled + unit / 2).div_euclid(unit)).unwrap_or(i64::MAX)
}

// ---------------------------------------------------------------- boxes

fn make_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let size = u32::try_from(body.len() + 8).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out
}

fn full_box(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut inner = Vec::with_capacity(body.len() + 4);
    inner.push(version);
    inner.extend_from_slice(&flags.to_be_bytes()[1..]);
    inner.extend_from_slice(body);
    make_box(kind, &inner)
}

fn concat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

#[derive(Default)]
struct W(Vec<u8>);

impl W {
    fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn i32(&mut self, v: i32) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn u64(&mut self, v: u64) -> &mut Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.0.extend_from_slice(v);
        self
    }
    fn zeros(&mut self, n: usize) -> &mut Self {
        self.0.resize(self.0.len() + n, 0);
        self
    }
}

const UNITY_MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];

fn u32_of(value: i64, what: &str) -> Result<u32, ReplayError> {
    u32::try_from(value).map_err(|_| invalid(format!("{what} out of range")))
}

fn count_u32(len: usize) -> Result<u32, ReplayError> {
    u32::try_from(len).map_err(|_| invalid("too many entries"))
}

/// `stco` when every offset fits 32 bits, otherwise `co64`.
pub(crate) fn chunk_offset_box(offsets: &[u64]) -> Result<Vec<u8>, ReplayError> {
    let wide = offsets.iter().any(|&offset| offset > u64::from(u32::MAX));
    let mut w = W::default();
    w.u32(count_u32(offsets.len())?);
    for &offset in offsets {
        if wide {
            w.u64(offset);
        } else {
            w.u32(offset as u32);
        }
    }
    Ok(full_box(if wide { b"co64" } else { b"stco" }, 0, 0, &w.0))
}

// -------------------------------------------------------------- timing

struct Timing {
    deltas: Vec<u32>,
    cts: Vec<u32>,
    media_duration: u64,
    media_time: i64,
}

fn timing(track: &TrackPlan, timescale: u32) -> Result<Timing, ReplayError> {
    let base = track.base();
    let count = track.samples.len();
    let mut stamps = Vec::with_capacity(count);
    let mut cts = Vec::with_capacity(count);
    for sample in &track.samples {
        let rel = track.decode_time(sample) - base;
        let dts = convert(rel, timescale);
        let pts = convert(rel + (sample.pts - track.decode_time(sample)), timescale);
        cts.push(u32_of(pts - dts, "composition offset")?);
        stamps.push(dts);
    }
    let mut deltas = Vec::with_capacity(count);
    for (index, sample) in track.samples.iter().enumerate() {
        let end = match stamps.get(index + 1) {
            Some(&next) => next,
            None => {
                let rel = track.decode_time(sample) - base;
                convert(rel + sample.duration, timescale)
            }
        };
        let delta = (end - stamps[index]).max(if index + 1 == count { 1 } else { 0 });
        deltas.push(u32_of(delta, "sample duration")?);
    }
    let media_duration = deltas.iter().map(|&d| u64::from(d)).sum();
    Ok(Timing {
        deltas,
        cts,
        media_duration,
        media_time: convert(track.media_time, timescale),
    })
}

fn run_length(values: &[u32]) -> Vec<(u32, u32)> {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for &value in values {
        match runs.last_mut() {
            Some((count, v)) if *v == value => *count += 1,
            _ => runs.push((1, value)),
        }
    }
    runs
}

// ------------------------------------------------------------- tracks

struct Chunk {
    first: usize,
    count: usize,
    offset: u64,
}

struct Built<'a> {
    plan: &'a TrackPlan,
    codec: &'a CodecConfig,
    timescale: u32,
    timing: Timing,
    chunks: Vec<Chunk>,
}

fn avc_config(sps: &[u8], pps: &[u8]) -> Result<Vec<u8>, ReplayError> {
    if sps.len() < 4 || pps.is_empty() {
        return Err(invalid("missing or truncated SPS/PPS"));
    }
    let sps_len = u16::try_from(sps.len()).map_err(|_| invalid("SPS too large"))?;
    let pps_len = u16::try_from(pps.len()).map_err(|_| invalid("PPS too large"))?;
    let mut w = W::default();
    w.u8(1).u8(sps[1]).u8(sps[2]).u8(sps[3]);
    w.u8(0xFF).u8(0xE1).u16(sps_len).bytes(sps);
    w.u8(1).u16(pps_len).bytes(pps);
    if matches!(sps[1], 100 | 110 | 122 | 144) {
        // 4:2:0, 8 bit luma/chroma (what the NV12 encoder path produces).
        w.u8(0xFD).u8(0xF8).u8(0xF8).u8(0);
    }
    Ok(make_box(b"avcC", &w.0))
}

fn descriptor(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let len = body.len();
    if len < 0x80 {
        out.push(len as u8);
    } else {
        // 4-byte expandable length (always valid, ISO 14496-1).
        out.extend_from_slice(&[
            0x80 | ((len >> 21) & 0x7F) as u8,
            0x80 | ((len >> 14) & 0x7F) as u8,
            0x80 | ((len >> 7) & 0x7F) as u8,
            (len & 0x7F) as u8,
        ]);
    }
    out.extend_from_slice(body);
    out
}

fn esds(track_id: u32, asc: &[u8]) -> Vec<u8> {
    let mut decoder = W::default();
    decoder
        .u8(0x40) // MPEG-4 audio
        .u8(0x15) // audio stream, reserved bit set
        .bytes(&[0, 0, 0]) // buffer size
        .u32(0)
        .u32(0)
        .bytes(&descriptor(0x05, asc));
    let mut es = W::default();
    es.u16(track_id as u16).u8(0);
    es.bytes(&descriptor(0x04, &decoder.0));
    es.bytes(&descriptor(0x06, &[2]));
    full_box(b"esds", 0, 0, &descriptor(0x03, &es.0))
}

fn sample_entry(track_id: u32, built: &Built<'_>) -> Result<Vec<u8>, ReplayError> {
    let mut w = W::default();
    w.zeros(6).u16(1);
    match built.codec {
        CodecConfig::H264 {
            width,
            height,
            sps,
            pps,
        } => {
            let (Ok(w16), Ok(h16)) = (u16::try_from(*width), u16::try_from(*height)) else {
                return Err(invalid("video dimensions out of range"));
            };
            w.zeros(16).u16(w16).u16(h16);
            w.u32(0x0048_0000).u32(0x0048_0000).u32(0).u16(1);
            w.zeros(32).u16(0x0018).u16(0xFFFF);
            w.bytes(&avc_config(sps, pps)?);
            Ok(make_box(b"avc1", &w.0))
        }
        CodecConfig::Aac {
            sample_rate,
            channels,
            audio_specific_config,
        } => {
            if audio_specific_config.is_empty() {
                return Err(invalid("missing AudioSpecificConfig"));
            }
            let rate = if *sample_rate <= 0xFFFF {
                *sample_rate
            } else {
                0
            };
            w.zeros(8)
                .u16(*channels)
                .u16(16)
                .u16(0)
                .u16(0)
                .u32(rate << 16);
            w.bytes(&esds(track_id, audio_specific_config));
            Ok(make_box(b"mp4a", &w.0))
        }
    }
}

fn stbl(track_id: u32, built: &Built<'_>) -> Result<Vec<u8>, ReplayError> {
    let samples = &built.plan.samples;
    let mut stsd = W::default();
    stsd.u32(1).bytes(&sample_entry(track_id, built)?);
    let mut parts = vec![full_box(b"stsd", 0, 0, &stsd.0)];

    let mut stts = W::default();
    let runs = run_length(&built.timing.deltas);
    stts.u32(count_u32(runs.len())?);
    for (count, delta) in &runs {
        stts.u32(*count).u32(*delta);
    }
    parts.push(full_box(b"stts", 0, 0, &stts.0));

    if built.plan.is_video() {
        let keys: Vec<u32> = samples
            .iter()
            .enumerate()
            .filter(|(_, sample)| sample.keyframe)
            .map(|(index, _)| index as u32 + 1)
            .collect();
        let mut stss = W::default();
        stss.u32(count_u32(keys.len())?);
        for key in keys {
            stss.u32(key);
        }
        parts.push(full_box(b"stss", 0, 0, &stss.0));
        if built.timing.cts.iter().any(|&offset| offset != 0) {
            let runs = run_length(&built.timing.cts);
            let mut ctts = W::default();
            ctts.u32(count_u32(runs.len())?);
            for (count, offset) in &runs {
                ctts.u32(*count).u32(*offset);
            }
            parts.push(full_box(b"ctts", 0, 0, &ctts.0));
        }
    }

    let mut stsc = W::default();
    let mut entries: Vec<(u32, u32)> = Vec::new();
    for (index, chunk) in built.chunks.iter().enumerate() {
        let per_chunk = count_u32(chunk.count)?;
        if entries.last().is_none_or(|&(_, last)| last != per_chunk) {
            entries.push((count_u32(index + 1)?, per_chunk));
        }
    }
    stsc.u32(count_u32(entries.len())?);
    for (first, per_chunk) in entries {
        stsc.u32(first).u32(per_chunk).u32(1);
    }
    parts.push(full_box(b"stsc", 0, 0, &stsc.0));

    let mut stsz = W::default();
    stsz.u32(0).u32(count_u32(samples.len())?);
    for sample in samples {
        stsz.u32(count_u32(sample.data.len())?);
    }
    parts.push(full_box(b"stsz", 0, 0, &stsz.0));

    let offsets: Vec<u64> = built.chunks.iter().map(|chunk| chunk.offset).collect();
    parts.push(chunk_offset_box(&offsets)?);

    let refs: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
    Ok(make_box(b"stbl", &refs.concat()))
}

fn edit_list(built: &Built<'_>) -> Vec<u8> {
    let plan = built.plan;
    let total = convert(plan.edit_duration, MOVIE_TIMESCALE);
    let lead = convert(plan.lead, MOVIE_TIMESCALE).min(total);
    let mut entries: Vec<(u64, i64)> = Vec::new();
    if lead > 0 {
        entries.push((lead as u64, -1));
    }
    entries.push(((total - lead).max(0) as u64, built.timing.media_time));
    let wide = entries
        .iter()
        .any(|&(dur, media)| dur > u64::from(u32::MAX) || media > i64::from(i32::MAX));
    let mut w = W::default();
    w.u32(entries.len() as u32);
    for (duration, media_time) in entries {
        if wide {
            w.u64(duration).u64(media_time as u64);
        } else {
            w.u32(duration as u32).i32(media_time as i32);
        }
        w.u16(1).u16(0);
    }
    let elst = full_box(b"elst", u8::from(wide), 0, &w.0);
    make_box(b"edts", &elst)
}

fn trak(track_id: u32, index: usize, built: &Built<'_>) -> Result<Vec<u8>, ReplayError> {
    let movie_duration = convert(built.plan.edit_duration, MOVIE_TIMESCALE);
    let video = built.plan.is_video();

    let flags = if video || index == 1 { 0x7 } else { 0x1 };
    let mut tkhd = W::default();
    tkhd.u32(0).u32(0).u32(track_id).u32(0);
    tkhd.u32(u32_of(movie_duration, "track duration")?);
    tkhd.zeros(8).u16(0);
    tkhd.u16(if video { 0 } else { 1 }); // alternate group
    tkhd.u16(if video { 0 } else { 0x0100 }).u16(0);
    for value in UNITY_MATRIX {
        tkhd.u32(value);
    }
    if let CodecConfig::H264 { width, height, .. } = built.codec {
        tkhd.u32(*width << 16).u32(*height << 16);
    } else {
        tkhd.u32(0).u32(0);
    }
    let tkhd = full_box(b"tkhd", 0, flags, &tkhd.0);

    let mut mdhd = W::default();
    mdhd.u32(0).u32(0).u32(built.timescale);
    mdhd.u32(u32::try_from(built.timing.media_duration).map_err(|_| invalid("media too long"))?);
    mdhd.u16(0x55C4).u16(0); // "und"
    let mdhd = full_box(b"mdhd", 0, 0, &mdhd.0);

    let mut hdlr = W::default();
    hdlr.u32(0)
        .bytes(if video { b"vide" } else { b"soun" })
        .zeros(12);
    hdlr.bytes(if video {
        b"VideoHandler\0"
    } else {
        b"SoundHandler\0"
    });
    let hdlr = full_box(b"hdlr", 0, 0, &hdlr.0);

    let media_header = if video {
        full_box(b"vmhd", 0, 1, &[0; 8])
    } else {
        full_box(b"smhd", 0, 0, &[0; 4])
    };
    let url = full_box(b"url ", 0, 1, &[]);
    let mut dref = W::default();
    dref.u32(1).bytes(&url);
    let dinf = make_box(b"dinf", &full_box(b"dref", 0, 0, &dref.0));
    let minf = make_box(
        b"minf",
        &concat(&[&media_header, &dinf, &stbl(track_id, built)?]),
    );
    let mdia = make_box(b"mdia", &concat(&[&mdhd, &hdlr, &minf]));
    Ok(make_box(
        b"trak",
        &concat(&[&tkhd, &edit_list(built), &mdia]),
    ))
}

// -------------------------------------------------------------- writer

/// Interleaved chunk order across tracks: `(track index, chunk)`.
fn layout_chunks(
    tracks: &mut [Built<'_>],
    start_pts: Ticks,
    data_start: u64,
) -> Vec<(usize, usize)> {
    let keys: Vec<Vec<Ticks>> = tracks
        .iter()
        .map(|t| {
            t.plan
                .samples
                .iter()
                .map(|s| t.plan.decode_time(s) - start_pts)
                .collect()
        })
        .collect();
    let bucket = |key: Ticks| key.max(0) / CHUNK_TICKS;
    let mut cursors = vec![0usize; tracks.len()];
    let mut order = Vec::new();
    let mut offset = data_start;
    while let Some(next) = keys
        .iter()
        .zip(&cursors)
        .filter_map(|(k, &c)| k.get(c).map(|&key| bucket(key)))
        .min()
    {
        for (index, track) in tracks.iter_mut().enumerate() {
            let first = cursors[index];
            let mut end = first;
            while end < keys[index].len() && bucket(keys[index][end]) <= next {
                end += 1;
            }
            if end > first {
                let bytes: u64 = track.plan.samples[first..end]
                    .iter()
                    .map(|s| s.data.len() as u64)
                    .sum();
                order.push((index, track.chunks.len()));
                track.chunks.push(Chunk {
                    first,
                    count: end - first,
                    offset,
                });
                offset += bytes;
                cursors[index] = end;
            }
        }
    }
    order
}

const FTYP: &[u8] = &{
    let mut out = [0u8; 28];
    out[3] = 28;
    out[4] = b'f';
    out[5] = b't';
    out[6] = b'y';
    out[7] = b'p';
    out[8] = b'i';
    out[9] = b's';
    out[10] = b'o';
    out[11] = b'm';
    out[14] = 0x02; // minor version 512
    let brands = *b"isomiso2avc1mp41";
    let mut i = 0;
    while i < 16 {
        out[12 + i] = brands[i];
        i += 1;
    }
    out
};

/// Writes `plan` as a complete MP4 to `out` and returns the bytes written.
/// `path` is only used for I/O error context.
pub fn write_mp4<W2: Write>(
    out: &mut W2,
    path: &Path,
    layout: &StreamLayout,
    plan: &ClipPlan,
) -> Result<u64, ReplayError> {
    let mut tracks: Vec<Built<'_>> = Vec::new();
    for track in plan.tracks() {
        if track.samples.is_empty() {
            return Err(invalid("track without samples"));
        }
        let codec = &layout
            .tracks
            .get(usize::from(track.track))
            .ok_or_else(|| invalid("plan references unknown track"))?
            .codec;
        let timescale = match (codec, track.is_video()) {
            (CodecConfig::H264 { .. }, true) => VIDEO_TIMESCALE,
            (CodecConfig::Aac { sample_rate, .. }, false) if *sample_rate > 0 => *sample_rate,
            _ => return Err(invalid("track codec does not match its role")),
        };
        tracks.push(Built {
            plan: track,
            codec,
            timescale,
            timing: timing(track, timescale)?,
            chunks: Vec::new(),
        });
    }

    let payload = plan.payload_bytes();
    let wide_mdat = payload + 8 > u64::from(u32::MAX);
    let header_len: u64 = if wide_mdat { 16 } else { 8 };
    let data_start = FTYP.len() as u64 + header_len;
    let order = layout_chunks(&mut tracks, plan.start_pts, data_start);

    let mvhd = {
        let duration = tracks
            .iter()
            .map(|t| convert(t.plan.edit_duration, MOVIE_TIMESCALE))
            .max()
            .unwrap_or(0);
        let mut w = W::default();
        w.u32(0).u32(0).u32(MOVIE_TIMESCALE);
        w.u32(u32_of(duration, "movie duration")?);
        w.u32(0x0001_0000).u16(0x0100).zeros(10);
        for value in UNITY_MATRIX {
            w.u32(value);
        }
        w.zeros(24).u32(count_u32(tracks.len())? + 1);
        full_box(b"mvhd", 0, 0, &w.0)
    };
    let mut moov_parts = vec![mvhd];
    for (index, built) in tracks.iter().enumerate() {
        moov_parts.push(trak(count_u32(index + 1)?, index, built)?);
    }
    let refs: Vec<&[u8]> = moov_parts.iter().map(Vec::as_slice).collect();
    let moov = make_box(b"moov", &refs.concat());
    if moov.len() > u32::MAX as usize {
        return Err(invalid("moov too large"));
    }

    let io = |err| ReplayError::io("writing clip", path, err);
    let mut written = 0u64;
    let mut put = |out: &mut W2, bytes: &[u8]| -> Result<(), ReplayError> {
        out.write_all(bytes).map_err(io)?;
        written += bytes.len() as u64;
        Ok(())
    };
    put(out, FTYP)?;
    if wide_mdat {
        let mut header = W::default();
        header.u32(1).bytes(b"mdat").u64(payload + 16);
        put(out, &header.0)?;
    } else {
        let mut header = W::default();
        header.u32(payload as u32 + 8).bytes(b"mdat");
        put(out, &header.0)?;
    }
    for (track, chunk) in order {
        let built = &tracks[track];
        let chunk = &built.chunks[chunk];
        for sample in &built.plan.samples[chunk.first..chunk.first + chunk.count] {
            put(out, &sample.data)?;
        }
    }
    put(out, &moov)?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::test_support::*;
    use crate::plan::{plan_clip, test_support};

    struct Bx<'a> {
        kind: String,
        /// Absolute offset of the box payload in the file.
        start: usize,
        body: &'a [u8],
    }

    fn walk(file: &[u8], start: usize, end: usize) -> Vec<Bx<'_>> {
        let mut out = Vec::new();
        let mut pos = start;
        while pos + 8 <= end {
            let mut size = u32::from_be_bytes(file[pos..pos + 4].try_into().unwrap()) as usize;
            let kind = String::from_utf8_lossy(&file[pos + 4..pos + 8]).into_owned();
            let mut head = 8;
            if size == 1 {
                size = u64::from_be_bytes(file[pos + 8..pos + 16].try_into().unwrap()) as usize;
                head = 16;
            }
            assert!(size >= head && pos + size <= end, "bad box {kind}");
            out.push(Bx {
                kind,
                start: pos + head,
                body: &file[pos + head..pos + size],
            });
            pos += size;
        }
        assert_eq!(pos, end, "trailing bytes");
        out
    }

    fn child<'a>(file: &'a [u8], parent: &Bx<'a>, kind: &str) -> Bx<'a> {
        walk(file, parent.start, parent.start + parent.body.len())
            .into_iter()
            .find(|b| b.kind == kind)
            .unwrap_or_else(|| panic!("missing {kind}"))
    }

    fn children<'a>(file: &'a [u8], parent: &Bx<'a>, kind: &str) -> Vec<Bx<'a>> {
        walk(file, parent.start, parent.start + parent.body.len())
            .into_iter()
            .filter(|b| b.kind == kind)
            .collect()
    }

    fn be32(b: &[u8], at: usize) -> u32 {
        u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
    }

    fn mux(plan: &ClipPlan, audio: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let n = write_mp4(&mut out, Path::new("x.mp4"), &layout(audio), plan).unwrap();
        assert_eq!(n as usize, out.len());
        out
    }

    /// Returns (sample sizes, chunk offsets, stts, ctts, stss) for a trak.
    struct Tables {
        sizes: Vec<u32>,
        offsets: Vec<u64>,
        stts: Vec<(u32, u32)>,
        ctts: Vec<(u32, u32)>,
        stss: Vec<u32>,
        stsc: Vec<(u32, u32)>,
    }

    fn tables(file: &[u8], trak: &Bx<'_>) -> Tables {
        let mdia = child(file, trak, "mdia");
        let minf = child(file, &mdia, "minf");
        let stbl = child(file, &minf, "stbl");
        let pairs = |kind: &str| -> Vec<(u32, u32)> {
            walk(file, stbl.start, stbl.start + stbl.body.len())
                .iter()
                .find(|b| b.kind == kind)
                .map(|b| {
                    let n = be32(b.body, 4) as usize;
                    (0..n)
                        .map(|i| (be32(b.body, 8 + i * 8), be32(b.body, 12 + i * 8)))
                        .collect()
                })
                .unwrap_or_default()
        };
        let stsz = child(file, &stbl, "stsz");
        let count = be32(stsz.body, 8) as usize;
        let sizes = (0..count).map(|i| be32(stsz.body, 12 + i * 4)).collect();
        let stco = child(file, &stbl, "stco");
        let n = be32(stco.body, 4) as usize;
        let offsets = (0..n)
            .map(|i| u64::from(be32(stco.body, 8 + i * 4)))
            .collect();
        let stss = walk(file, stbl.start, stbl.start + stbl.body.len())
            .iter()
            .find(|b| b.kind == "stss")
            .map(|b| {
                (0..be32(b.body, 4) as usize)
                    .map(|i| be32(b.body, 8 + i * 4))
                    .collect()
            })
            .unwrap_or_default();
        let stsc = {
            let b = child(file, &stbl, "stsc");
            (0..be32(b.body, 4) as usize)
                .map(|i| (be32(b.body, 8 + i * 12), be32(b.body, 12 + i * 12)))
                .collect()
        };
        Tables {
            sizes,
            offsets,
            stts: pairs("stts"),
            ctts: pairs("ctts"),
            stss,
            stsc,
        }
    }

    /// Sample payload offsets for a track given its stsc/stco/stsz.
    fn sample_offsets(t: &Tables) -> Vec<(u64, u32)> {
        let mut out = Vec::new();
        let mut index = 0usize;
        for (chunk, &offset) in t.offsets.iter().enumerate() {
            let per = t
                .stsc
                .iter()
                .rev()
                .find(|(first, _)| *first as usize <= chunk + 1)
                .unwrap()
                .1 as usize;
            let mut pos = offset;
            for _ in 0..per {
                out.push((pos, t.sizes[index]));
                pos += u64::from(t.sizes[index]);
                index += 1;
            }
        }
        assert_eq!(index, t.sizes.len());
        out
    }

    #[test]
    fn box_structure_offsets_and_durations() {
        let layout = layout(2);
        let plan = plan_clip(&layout, &simple_gops(2), ms(2000), ms(6000)).unwrap();
        let file = mux(&plan, 2);
        let top = walk(&file, 0, file.len());
        let kinds: Vec<_> = top.iter().map(|b| b.kind.as_str()).collect();
        assert_eq!(kinds, ["ftyp", "mdat", "moov"]);
        assert_eq!(&top[0].body[..4], b"isom");
        let mdat = &top[1];
        let moov = &top[2];
        let traks = children(&file, moov, "trak");
        assert_eq!(traks.len(), 3);

        let mvhd = child(&file, moov, "mvhd");
        assert_eq!(be32(mvhd.body, 12), 1000);
        assert_eq!(be32(mvhd.body, 16), 2000);

        let mdat_range = mdat.start..mdat.start + mdat.body.len();
        for (index, trak) in traks.iter().enumerate() {
            let t = tables(&file, trak);
            let track_plan = plan.tracks().nth(index).unwrap();
            assert_eq!(t.sizes.len(), track_plan.samples.len());
            let offsets = sample_offsets(&t);
            for ((offset, size), sample) in offsets.iter().zip(&track_plan.samples) {
                assert!(mdat_range.contains(&(*offset as usize)));
                let at = *offset as usize;
                assert_eq!(&file[at..at + *size as usize], &sample.data[..]);
            }
            let total: u64 = t
                .stts
                .iter()
                .map(|(c, d)| u64::from(*c) * u64::from(*d))
                .sum();
            let mdhd = child(&file, &child(&file, trak, "mdia"), "mdhd");
            assert_eq!(u64::from(be32(mdhd.body, 16)), total);
            let tkhd = child(&file, trak, "tkhd");
            assert_eq!(be32(tkhd.body, 20), 2000.min(be32(tkhd.body, 20)));
            if index == 0 {
                assert_eq!(total, 2000 * 90);
                assert_eq!(t.stss, [1]);
                assert!(t.ctts.is_empty());
                assert_eq!(t.stts, [(20, 9000)]);
                assert_eq!(be32(mdhd.body, 12), VIDEO_TIMESCALE);
                assert_eq!(be32(tkhd.body, 0) & 0xFF_FFFF, 7);
                assert_eq!(be32(tkhd.body, 76), 1280 << 16);
            } else {
                assert_eq!(be32(mdhd.body, 12), 48_000);
                assert!(t.stts.iter().all(|(_, d)| *d == 1024));
                let flags = be32(tkhd.body, 0) & 0xFF_FFFF;
                assert_eq!(flags, if index == 1 { 7 } else { 1 });
                // alternate_group = 1 for all audio
                assert_eq!(u16::from_be_bytes([tkhd.body[34], tkhd.body[35]]), 1);
            }
        }
        // interleaved: video and audio chunk offsets alternate in file order
        let v = tables(&file, &traks[0]);
        let a = tables(&file, &traks[1]);
        assert_eq!(v.offsets.len(), a.offsets.len());
        assert!(v.offsets[0] < a.offsets[0] && a.offsets[0] < v.offsets[1]);
    }

    #[test]
    fn b_frames_write_ctts_and_edit_list() {
        let layout = layout(0);
        let plan = plan_clip(&layout, &test_support_b_frames(), ms(500), ms(2900)).unwrap();
        let file = mux(&plan, 0);
        let top = walk(&file, 0, file.len());
        let trak = child(&file, &top[2], "trak");
        let t = tables(&file, &trak);
        // dts 0,100,..600 ms -> 9000 per 100 ms; last frame 100 ms
        assert_eq!(t.stts, [(7, 9000)]);
        assert!(!t.ctts.is_empty());
        let cts: Vec<u32> = t
            .ctts
            .iter()
            .flat_map(|(c, o)| vec![*o; *c as usize])
            .collect();
        // pts - dts: 200,400,100,100,400,100,100 ms
        let expected: Vec<u32> = [200, 400, 100, 100, 400, 100, 100]
            .iter()
            .map(|v| v * 90)
            .collect();
        assert_eq!(cts, expected);
        let elst = child(&file, &child(&file, &trak, "edts"), "elst");
        assert_eq!(be32(elst.body, 4), 1);
        assert_eq!(be32(elst.body, 8), 700); // segment duration, ms
        assert_eq!(be32(elst.body, 12), 200 * 90); // media_time
        // earliest presentation = media_time: min(dts+cts) - 0 == media_time
        let min_pts = (0..7u32)
            .map(|i| i * 9000 + expected[i as usize])
            .min()
            .unwrap();
        assert_eq!(min_pts, 200 * 90);
    }

    fn test_support_b_frames() -> Vec<std::sync::Arc<crate::gop::Gop>> {
        let order = [
            (0, 200),
            (100, 500),
            (200, 300),
            (300, 400),
            (400, 800),
            (500, 600),
            (600, 700),
        ];
        (0..2)
            .map(|g| {
                let base = g * 2000;
                let packets = order
                    .iter()
                    .enumerate()
                    .map(|(i, (d, p))| video_pkt(base + p, base + d, i == 0))
                    .collect();
                gop(g as u64, packets)
            })
            .collect()
    }

    #[test]
    fn audio_media_time_and_lead_in_edit_list() {
        let mut gops = simple_gops(1);
        let mut packets = gops[2].packets.clone();
        packets.retain(|p| p.track == 0 || p.pts >= ms(4500));
        let mut prev = gops[1].packets.clone();
        prev.retain(|p| p.track == 0 || p.pts < ms(3700));
        gops[1] = gop(1, prev);
        gops[2] = gop(2, packets);
        let plan = plan_clip(&layout(1), &gops, ms(2000), ms(6000)).unwrap();
        let file = mux(&plan, 1);
        let top = walk(&file, 0, file.len());
        let traks = children(&file, &top[2], "trak");
        let elst = child(&file, &child(&file, &traks[1], "edts"), "elst");
        assert_eq!(be32(elst.body, 4), 2);
        assert_eq!(be32(elst.body, 12) as i32, -1);
        assert!(be32(elst.body, 8) > 0 && be32(elst.body, 8) <= 520);
    }

    #[test]
    fn chunk_offsets_switch_to_co64() {
        let narrow = chunk_offset_box(&[8, u64::from(u32::MAX)]).unwrap();
        assert_eq!(&narrow[4..8], b"stco");
        assert_eq!(narrow.len(), 8 + 8 + 8);
        let wide = chunk_offset_box(&[8, u64::from(u32::MAX) + 1]).unwrap();
        assert_eq!(&wide[4..8], b"co64");
        assert_eq!(wide.len(), 8 + 8 + 16);
        assert_eq!(
            u64::from_be_bytes(wide[24..32].try_into().unwrap()),
            u64::from(u32::MAX) + 1
        );
    }

    #[test]
    fn avcc_and_esds_contents() {
        let plan = plan_clip(&layout(1), &simple_gops(1), ms(2000), ms(6000)).unwrap();
        let file = mux(&plan, 1);
        let find = |needle: &[u8]| file.windows(needle.len()).position(|w| w == needle);
        let avcc = find(b"avcC").unwrap() + 4;
        assert_eq!(&file[avcc..avcc + 7], &[1, 0x64, 0x00, 0x28, 0xFF, 0xE1, 0]);
        assert_eq!(file[avcc + 7], 7);
        let esds = find(b"esds").unwrap();
        assert!(
            file[esds..esds + 40]
                .windows(4)
                .any(|w| w == [0x05, 0x02, 0x11, 0x90])
        );
        assert!(file[esds..esds + 40].windows(2).any(|w| w == [0x04, 0x11]));
    }

    #[test]
    fn rejects_bad_parameter_sets() {
        let mut bad = layout(0);
        bad.tracks[0].codec = CodecConfig::H264 {
            width: 16,
            height: 16,
            sps: vec![0x67],
            pps: vec![0x68],
        };
        let plan = plan_clip(&layout(0), &simple_gops(0), ms(2000), ms(6000)).unwrap();
        let mut out = Vec::new();
        let result = write_mp4(&mut out, Path::new("x"), &bad, &plan);
        assert!(matches!(result, Err(ReplayError::Invalid(_))));
    }

    #[test]
    fn rounding_keeps_durations_summing() {
        assert_eq!(convert(213_333, 48_000), 1024);
        assert_eq!(convert(166_667, 90_000), 1500);
        assert_eq!(convert(-1, 90_000), 0);
        let _ = test_support::AUDIO_DUR;
    }
}
