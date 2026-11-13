//! SSD tier: rotating temporary segment files.
//!
//! Layout: `<root>/session-<pid>-<unix_millis>/` holds one `lock` file (kept
//! open and, on Windows, opened with share mode 0 so no other process can
//! open or delete it) plus `seg-<first>-<last>.seg` segment files.
//!
//! Lock limitation: on non-Windows targets the lock is only an open file
//! handle (no `flock`, no new dependencies), so cleanup in another process
//! cannot detect a live owner. Cleanup there only protects sessions of its
//! own process id. Clipper itself is Windows only.
//!
//! Segment file format (little endian, version 1):
//! ```text
//! file   := b"CLPSEG01" version:u32 record*
//! record := sequence:u64 start_pts:i64 end_pts:i64 charged_bytes:u64
//!           packet_count:u32 packet* crc32:u32      (crc over all prior record bytes)
//! packet := track:u8 keyframe:u8 pts:i64 dts:i64 duration:i64 len:u32 data
//! ```
//! Segments are temporary: they are written without `sync_data`, a crash
//! discards them anyway.

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::error::ReplayError;
use crate::gop::Gop;
use crate::packet::{EncodedPacket, PACKET_OVERHEAD, Ticks};

const FILE_MAGIC: &[u8; 8] = b"CLPSEG01";
const FORMAT_VERSION: u32 = 1;
const FILE_HEADER_LEN: u64 = 12;
/// sequence + start + end + charged + packet_count.
const RECORD_HEADER_LEN: u64 = 36;
/// track + keyframe + pts + dts + duration + len.
const PACKET_HEADER_LEN: u64 = 30;
const CRC_LEN: u64 = 4;
/// Free space that must remain on the volume after a segment write.
const SAFETY_MARGIN: u64 = 64 * 1024 * 1024;
/// A session dir without a lock file younger than this may still be starting.
const LOCKLESS_GRACE: Duration = Duration::from_secs(60);
const LOCK_NAME: &str = "lock";

/// Where and how much the SSD tier may write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskConfig {
    /// App-owned cache root, e.g. `%LOCALAPPDATA%\Clipper\replay-cache`.
    /// Each session writes into its own locked subdirectory of it.
    pub root: PathBuf,
    /// Upper bound for live segment bytes of this session.
    pub max_bytes: u64,
}

/// Index entry for one GOP stored inside a segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentGop {
    pub sequence: u64,
    pub start_pts: Ticks,
    pub end_pts: Ticks,
    /// Byte range of the GOP record inside the segment file.
    pub offset: u64,
    pub len: u64,
    /// `Gop::charged_bytes` of the original group.
    pub charged_bytes: usize,
}

/// Shared session state. When the last owner (store or segment) drops, the
/// lock is released and the session dir removed (best effort).
#[derive(Debug)]
struct Inner {
    dir: PathBuf,
    bytes_used: AtomicU64,
    lock: Option<File>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Close the handle first: Windows cannot delete an open share-0 file.
        drop(self.lock.take());
        let _ = fs::remove_file(self.dir.join(LOCK_NAME));
        let _ = fs::remove_dir(&self.dir);
    }
}

/// One finished, immutable segment file. Deleting is tied to `Drop`: the
/// file is removed when the buffer evicted it AND no export snapshot still
/// holds an `Arc<Segment>`, so a running export can never lose its input.
#[derive(Debug)]
pub struct Segment {
    path: PathBuf,
    bytes: u64,
    gops: Vec<SegmentGop>,
    inner: Arc<Inner>,
}

impl Segment {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// File size in bytes.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn gops(&self) -> &[SegmentGop] {
        &self.gops
    }

    /// Reads and validates (length, checksum) GOP `index` of `gops()`.
    pub fn read_gop(&self, index: usize) -> Result<Gop, ReplayError> {
        let entry = self
            .gops
            .get(index)
            .ok_or_else(|| ReplayError::Invalid(format!("gop index {index} out of range")))?;
        let corrupt = |what: &str| ReplayError::Corrupt(format!("{}: {what}", self.path.display()));
        if entry.len < RECORD_HEADER_LEN + CRC_LEN
            || entry.len > self.bytes
            || entry
                .offset
                .checked_add(entry.len)
                .is_none_or(|end| end > self.bytes)
        {
            return Err(corrupt("index entry out of bounds"));
        }
        let len = usize::try_from(entry.len).map_err(|_| corrupt("record too large"))?;

        let mut file = File::open(&self.path)
            .map_err(|e| ReplayError::io("opening replay segment", &self.path, e))?;
        file.seek(SeekFrom::Start(entry.offset))
            .map_err(|e| ReplayError::io("seeking replay segment", &self.path, e))?;
        let mut buf = vec![0u8; len];
        file.read_exact(&mut buf).map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                corrupt("truncated record")
            } else {
                ReplayError::io("reading replay segment", &self.path, e)
            }
        })?;

        let (body, tail) = buf.split_at(len - CRC_LEN as usize);
        let stored = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]);
        if crc32(body) != stored {
            return Err(corrupt("checksum mismatch"));
        }

        let mut r = Reader { buf: body };
        let sequence = r.u64().ok_or_else(|| corrupt("short header"))?;
        let start_pts = r.i64().ok_or_else(|| corrupt("short header"))?;
        let end_pts = r.i64().ok_or_else(|| corrupt("short header"))?;
        let charged = r.u64().ok_or_else(|| corrupt("short header"))?;
        let count = r.u32().ok_or_else(|| corrupt("short header"))? as u64;
        if sequence != entry.sequence
            || start_pts != entry.start_pts
            || end_pts != entry.end_pts
            || charged != entry.charged_bytes as u64
        {
            return Err(corrupt("header does not match index"));
        }
        // Each packet needs at least its fixed header: bounds the allocation.
        if count > (body.len() as u64) / PACKET_HEADER_LEN {
            return Err(corrupt("packet count exceeds record size"));
        }
        let mut packets = Vec::with_capacity(count as usize);
        let mut recomputed = 0usize;
        for _ in 0..count {
            let track = r.u8().ok_or_else(|| corrupt("short packet"))?;
            let keyframe = match r.u8().ok_or_else(|| corrupt("short packet"))? {
                0 => false,
                1 => true,
                _ => return Err(corrupt("bad keyframe flag")),
            };
            let pts = r.i64().ok_or_else(|| corrupt("short packet"))?;
            let dts = r.i64().ok_or_else(|| corrupt("short packet"))?;
            let duration = r.i64().ok_or_else(|| corrupt("short packet"))?;
            let dlen = r.u32().ok_or_else(|| corrupt("short packet"))? as usize;
            let data = r
                .take(dlen)
                .ok_or_else(|| corrupt("packet data out of bounds"))?;
            recomputed = recomputed.saturating_add(dlen + PACKET_OVERHEAD);
            packets.push(EncodedPacket {
                track,
                pts,
                dts,
                duration,
                keyframe,
                data: Arc::from(data),
            });
        }
        if !r.buf.is_empty() {
            return Err(corrupt("trailing bytes in record"));
        }
        if recomputed as u64 != charged {
            return Err(corrupt("charged bytes mismatch"));
        }
        Ok(Gop {
            sequence,
            start_pts,
            end_pts,
            packets,
            charged_bytes: recomputed,
        })
    }
}

impl Drop for Segment {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        self.inner
            .bytes_used
            .fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.buf.len() < n {
            return None;
        }
        let (head, rest) = self.buf.split_at(n);
        self.buf = rest;
        Some(head)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Option<u64> {
        self.take(8)
            .map(|b| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }
    fn i64(&mut self) -> Option<i64> {
        self.u64().map(|v| v as i64)
    }
}

const fn make_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

static CRC_TABLE: [u32; 256] = make_crc_table();

fn crc32_update(state: u32, data: &[u8]) -> u32 {
    let mut c = state;
    for &b in data {
        c = CRC_TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c
}

/// CRC-32/IEEE.
fn crc32(data: &[u8]) -> u32 {
    !crc32_update(!0, data)
}

/// Writer that checksums the current record while streaming it out.
struct RecordWriter {
    out: BufWriter<File>,
    crc: u32,
    pos: u64,
}

impl RecordWriter {
    fn put(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.crc = crc32_update(self.crc, data);
        self.pos += data.len() as u64;
        self.out.write_all(data)
    }
    fn begin_record(&mut self) {
        self.crc = !0;
    }
    fn end_record(&mut self) -> std::io::Result<()> {
        let crc = (!self.crc).to_le_bytes();
        self.pos += 4;
        self.out.write_all(&crc)
    }
}

fn record_size(gop: &Gop) -> u64 {
    RECORD_HEADER_LEN
        + gop
            .packets
            .iter()
            .map(|p| PACKET_HEADER_LEN + p.data.len() as u64)
            .sum::<u64>()
        + CRC_LEN
}

/// Writer for one replay session's segment directory. Lives on the spill
/// thread; never touched by capture or UI threads.
#[derive(Debug)]
pub struct SegmentStore {
    inner: Arc<Inner>,
    max_bytes: u64,
}

impl SegmentStore {
    /// Creates and locks `<root>/session-<id>/`, removing stale sessions of
    /// crashed processes first (only directories this module created).
    pub fn open(config: &DiskConfig) -> Result<Self, ReplayError> {
        fs::create_dir_all(&config.root)
            .map_err(|e| ReplayError::io("creating replay cache root", &config.root, e))?;
        // Stale cleanup is best effort: never block recording on it.
        let _ = cleanup_stale_sessions(&config.root);

        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let dir = config
            .root
            .join(format!("session-{}-{millis}", std::process::id()));
        fs::create_dir(&dir)
            .map_err(|e| ReplayError::io("creating replay session dir", &dir, e))?;
        let lock_path = dir.join(LOCK_NAME);
        let lock = match open_lock(&lock_path) {
            Ok(file) => file,
            Err(e) => {
                let _ = fs::remove_dir(&dir);
                return Err(ReplayError::io("locking replay session", &lock_path, e));
            }
        };
        Ok(Self {
            inner: Arc::new(Inner {
                dir,
                bytes_used: AtomicU64::new(0),
                lock: Some(lock),
            }),
            max_bytes: config.max_bytes,
        })
    }

    /// Writes `gops` (oldest first, contiguous sequences) as one segment:
    /// `*.tmp` then flush and atomic rename. Fails with `DiskFull` before
    /// writing when the session would exceed `max_bytes` or the volume is
    /// short on space; a failed write leaves no file behind.
    pub fn write_segment(&mut self, gops: &[Arc<Gop>]) -> Result<Arc<Segment>, ReplayError> {
        let (Some(first), Some(last)) = (gops.first(), gops.last()) else {
            return Err(ReplayError::Invalid("empty segment".into()));
        };
        let dir = &self.inner.dir;
        for pair in gops.windows(2) {
            if pair[1].sequence != pair[0].sequence.wrapping_add(1) {
                return Err(ReplayError::Invalid("non-contiguous gop sequences".into()));
            }
        }
        let mut size = FILE_HEADER_LEN;
        for gop in gops {
            if gop.packets.len() > u32::MAX as usize
                || gop.packets.iter().any(|p| p.data.len() > u32::MAX as usize)
            {
                return Err(ReplayError::Invalid("gop too large".into()));
            }
            size = size.saturating_add(record_size(gop));
        }
        if self
            .inner
            .bytes_used
            .load(Ordering::Acquire)
            .saturating_add(size)
            > self.max_bytes
        {
            return Err(ReplayError::DiskFull { path: dir.clone() });
        }
        if let Some(free) = available_space(dir)
            && free < size.saturating_add(SAFETY_MARGIN)
        {
            return Err(ReplayError::DiskFull { path: dir.clone() });
        }

        let name = format!("seg-{}-{}", first.sequence, last.sequence);
        let tmp_path = dir.join(format!("{name}.seg.tmp"));
        let final_path = dir.join(format!("{name}.seg"));

        let index = match write_file(&tmp_path, gops, size) {
            Ok(index) => index,
            Err(e) => {
                let _ = fs::remove_file(&tmp_path);
                return Err(ReplayError::io("writing replay segment", &tmp_path, e));
            }
        };
        if let Err(e) = fs::rename(&tmp_path, &final_path) {
            let _ = fs::remove_file(&tmp_path);
            return Err(ReplayError::io("finalizing replay segment", &final_path, e));
        }
        self.inner.bytes_used.fetch_add(size, Ordering::AcqRel);
        Ok(Arc::new(Segment {
            path: final_path,
            bytes: size,
            gops: index,
            inner: Arc::clone(&self.inner),
        }))
    }

    /// Live bytes of this session's segments, including segments only kept
    /// alive by running exports.
    pub fn bytes_used(&self) -> u64 {
        self.inner.bytes_used.load(Ordering::Acquire)
    }

    pub fn session_dir(&self) -> &Path {
        &self.inner.dir
    }
}

fn write_file(path: &Path, gops: &[Arc<Gop>], expected: u64) -> std::io::Result<Vec<SegmentGop>> {
    let file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let mut w = RecordWriter {
        out: BufWriter::with_capacity(256 * 1024, file),
        crc: !0,
        pos: 0,
    };
    w.out.write_all(FILE_MAGIC)?;
    w.out.write_all(&FORMAT_VERSION.to_le_bytes())?;
    w.pos = FILE_HEADER_LEN;

    let mut index = Vec::with_capacity(gops.len());
    for gop in gops {
        let offset = w.pos;
        w.begin_record();
        w.put(&gop.sequence.to_le_bytes())?;
        w.put(&gop.start_pts.to_le_bytes())?;
        w.put(&gop.end_pts.to_le_bytes())?;
        // Recompute so the stored value always matches the packets.
        let charged: usize = gop.packets.iter().map(EncodedPacket::charged_size).sum();
        w.put(&(charged as u64).to_le_bytes())?;
        w.put(&(gop.packets.len() as u32).to_le_bytes())?;
        for p in &gop.packets {
            w.put(&[p.track, u8::from(p.keyframe)])?;
            w.put(&p.pts.to_le_bytes())?;
            w.put(&p.dts.to_le_bytes())?;
            w.put(&p.duration.to_le_bytes())?;
            w.put(&(p.data.len() as u32).to_le_bytes())?;
            w.put(&p.data)?;
        }
        w.end_record()?;
        index.push(SegmentGop {
            sequence: gop.sequence,
            start_pts: gop.start_pts,
            end_pts: gop.end_pts,
            offset,
            len: w.pos - offset,
            charged_bytes: charged,
        });
    }
    w.out.flush()?;
    debug_assert_eq!(w.pos, expected);
    if w.pos != expected {
        return Err(std::io::Error::other("segment size mismatch"));
    }
    Ok(index)
}

#[cfg(windows)]
fn open_lock(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(0)
        .open(path)
}

#[cfg(not(windows))]
fn open_lock(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

/// True for symlinks and (on Windows) any reparse point such as junctions.
fn is_link(meta: &fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

fn is_own_file(name: &str) -> bool {
    name == LOCK_NAME || name.ends_with(".seg") || name.ends_with(".seg.tmp")
}

/// Removes session directories under `root` whose owning process is gone
/// (lock not held). Never touches anything outside `root` or files it did
/// not create. Returns the number of removed sessions.
pub fn cleanup_stale_sessions(root: &Path) -> Result<usize, ReplayError> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(ReplayError::io("listing replay cache root", root, e)),
    };
    let own_prefix = format!("session-{}-", std::process::id());
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("session-") {
            continue;
        }
        let dir = entry.path();
        let Ok(meta) = fs::symlink_metadata(&dir) else {
            continue;
        };
        if !meta.is_dir() || is_link(&meta) {
            continue;
        }
        // Without OS locking we cannot see our own process' live sessions.
        if cfg!(not(windows)) && name.starts_with(&own_prefix) {
            continue;
        }
        if remove_stale_session(&dir, &meta) {
            removed += 1;
        }
    }
    Ok(removed)
}

fn remove_stale_session(dir: &Path, dir_meta: &fs::Metadata) -> bool {
    let lock = dir.join(LOCK_NAME);
    match fs::symlink_metadata(&lock) {
        Ok(m) if m.is_file() && !is_link(&m) => {
            // Windows: fails while a live owner holds the share-0 handle.
            if fs::remove_file(&lock).is_err() {
                return false;
            }
        }
        Ok(_) => return false,
        Err(_) => {
            // No lock: either an orphan or a session that is still starting.
            let young = dir_meta
                .modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_none_or(|age| age < LOCKLESS_GRACE);
            if young {
                return false;
            }
        }
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !is_own_file(name) {
            continue;
        }
        if let Ok(m) = fs::symlink_metadata(entry.path())
            && m.is_file()
            && !is_link(&m)
        {
            let _ = fs::remove_file(entry.path());
        }
    }
    // Fails (and keeps the dir) when foreign files remain.
    fs::remove_dir(dir).is_ok()
}

/// Free bytes available to the caller on the volume containing `path`.
pub fn available_space(path: &Path) -> Option<u64> {
    #[cfg(windows)]
    {
        // The path itself may not exist yet: use the nearest existing ancestor.
        path.ancestors()
            .filter(|p| !p.as_os_str().is_empty())
            .find_map(free_bytes_windows)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

#[cfg(windows)]
fn free_bytes_windows(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0u64;
    // SAFETY: `wide` is a NUL terminated UTF-16 string that outlives the call
    // and `free` is a valid, writable u64; the other out pointers are None.
    #[allow(unsafe_code)]
    let result = unsafe {
        windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            windows::core::PCWSTR(wide.as_ptr()),
            Some(&mut free),
            None,
            None,
        )
    };
    result.ok().map(|()| free)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    struct TempRoot(PathBuf);
    impl TempRoot {
        fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!(
                "clipper-disk-test-{}-{n}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos())
            ));
            Self(p)
        }
    }
    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn pkt(track: u8, keyframe: bool, pts: i64, len: usize, fill: u8) -> EncodedPacket {
        EncodedPacket {
            track,
            pts,
            dts: pts - 1,
            duration: 100,
            keyframe,
            data: Arc::from(vec![fill; len]),
        }
    }

    fn gop(seq: u64, base: i64) -> Arc<Gop> {
        let mut g = Gop::new(seq, pkt(0, true, base, 500 + seq as usize, seq as u8));
        g.push(pkt(1, true, base + 10, 40, 7));
        g.push(pkt(0, false, base + 50, 0, 0));
        g.push(pkt(2, true, base + 60, 13, 9));
        Arc::new(g)
    }

    fn store(root: &TempRoot, max: u64) -> SegmentStore {
        SegmentStore::open(&DiskConfig {
            root: root.0.clone(),
            max_bytes: max,
        })
        .unwrap()
    }

    fn files_with_ext(dir: &Path, suffix: &str) -> usize {
        fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(suffix))
            .count()
    }

    #[test]
    fn crc_known_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn round_trip_byte_exact() {
        let root = TempRoot::new();
        let mut s = store(&root, 1 << 30);
        let gops: Vec<_> = (3..6).map(|i| gop(i, i as i64 * 1000)).collect();
        let seg = s.write_segment(&gops).unwrap();
        assert_eq!(seg.gops().len(), 3);
        assert_eq!(seg.bytes(), fs::metadata(seg.path()).unwrap().len());
        for (i, g) in gops.iter().enumerate() {
            let back = seg.read_gop(i).unwrap();
            assert_eq!(&back, g.as_ref());
            assert_eq!(back.charged_bytes, g.charged_bytes);
            assert_eq!(seg.gops()[i].charged_bytes, g.charged_bytes);
        }
        assert!(seg.read_gop(3).is_err());
    }

    #[test]
    fn empty_input_invalid() {
        let root = TempRoot::new();
        let mut s = store(&root, 1 << 30);
        assert!(matches!(s.write_segment(&[]), Err(ReplayError::Invalid(_))));
    }

    #[test]
    fn deleted_when_last_arc_drops_and_deferred_for_clone() {
        let root = TempRoot::new();
        let mut s = store(&root, 1 << 30);
        let seg = s.write_segment(&[gop(0, 0)]).unwrap();
        let path = seg.path().to_path_buf();
        let export = Arc::clone(&seg);
        assert_eq!(s.bytes_used(), seg.bytes());
        drop(seg);
        assert!(path.exists());
        assert!(export.read_gop(0).is_ok());
        assert_eq!(s.bytes_used(), export.bytes());
        drop(export);
        assert!(!path.exists());
        assert_eq!(s.bytes_used(), 0);
    }

    #[test]
    fn session_dir_removed_after_store_and_segments_gone() {
        let root = TempRoot::new();
        let mut s = store(&root, 1 << 30);
        let dir = s.session_dir().to_path_buf();
        let seg = s.write_segment(&[gop(0, 0)]).unwrap();
        drop(s);
        assert!(dir.exists());
        drop(seg);
        assert!(!dir.exists());
    }

    #[test]
    fn max_bytes_limit_disk_full_no_tmp() {
        let root = TempRoot::new();
        let mut s = store(&root, 600);
        let err = s.write_segment(&[gop(0, 0)]).unwrap_err();
        assert!(matches!(err, ReplayError::DiskFull { .. }));
        assert_eq!(files_with_ext(s.session_dir(), ".tmp"), 0);
        assert_eq!(files_with_ext(s.session_dir(), ".seg"), 0);
        assert_eq!(s.bytes_used(), 0);
    }

    #[test]
    fn corrupt_byte_flip_detected() {
        let root = TempRoot::new();
        let mut s = store(&root, 1 << 30);
        let seg = s.write_segment(&[gop(0, 0)]).unwrap();
        let mut data = fs::read(seg.path()).unwrap();
        let mid = data.len() / 2;
        data[mid] ^= 0xFF;
        fs::write(seg.path(), data).unwrap();
        assert!(matches!(seg.read_gop(0), Err(ReplayError::Corrupt(_))));
    }

    #[test]
    fn truncated_file_detected() {
        let root = TempRoot::new();
        let mut s = store(&root, 1 << 30);
        let seg = s.write_segment(&[gop(0, 0), gop(1, 1000)]).unwrap();
        let data = fs::read(seg.path()).unwrap();
        fs::write(seg.path(), &data[..data.len() - 20]).unwrap();
        assert!(seg.read_gop(0).is_ok());
        assert!(matches!(seg.read_gop(1), Err(ReplayError::Corrupt(_))));
    }

    #[test]
    fn stale_cleanup_only_removes_unlocked_own_files() {
        let root = TempRoot::new();
        fs::create_dir_all(&root.0).unwrap();
        let live = store(&root, 1 << 30);

        let stale = root.0.join("session-999999-1");
        fs::create_dir(&stale).unwrap();
        fs::write(stale.join("lock"), b"").unwrap();
        fs::write(stale.join("seg-0-0.seg"), b"x").unwrap();
        fs::write(stale.join("seg-1-1.seg.tmp"), b"x").unwrap();

        let mixed = root.0.join("session-999998-1");
        fs::create_dir(&mixed).unwrap();
        fs::write(mixed.join("lock"), b"").unwrap();
        fs::write(mixed.join("a.seg"), b"x").unwrap();
        fs::write(mixed.join("keep.txt"), b"mine").unwrap();

        fs::write(root.0.join("keep.txt"), b"mine").unwrap();
        let foreign = root.0.join("other");
        fs::create_dir(&foreign).unwrap();
        fs::write(foreign.join("x.seg"), b"x").unwrap();

        let n = cleanup_stale_sessions(&root.0).unwrap();
        assert_eq!(n, 1);
        assert!(!stale.exists());
        assert!(mixed.exists());
        assert!(mixed.join("keep.txt").exists());
        assert!(!mixed.join("a.seg").exists());
        assert!(!mixed.join("lock").exists());
        assert!(root.0.join("keep.txt").exists());
        assert!(foreign.join("x.seg").exists());
        assert!(live.session_dir().join("lock").exists());
    }

    #[test]
    fn open_on_bad_root_errors_cleanly() {
        let root = TempRoot::new();
        fs::create_dir_all(&root.0).unwrap();
        let file = root.0.join("afile");
        fs::write(&file, b"x").unwrap();
        let res = SegmentStore::open(&DiskConfig {
            root: file.join("sub"),
            max_bytes: 1 << 20,
        });
        assert!(res.is_err());
    }

    #[test]
    fn write_into_vanished_dir_errors_cleanly() {
        let root = TempRoot::new();
        let mut s = store(&root, 1 << 30);
        // A directory squatting on the tmp name makes the create fail.
        let blocker = s.session_dir().join("seg-0-0.seg.tmp");
        fs::create_dir(&blocker).unwrap();
        assert!(s.write_segment(&[gop(0, 0)]).is_err());
        assert_eq!(s.bytes_used(), 0);
        fs::remove_dir(&blocker).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn available_space_known_on_windows() {
        let root = TempRoot::new();
        assert!(available_space(&std::env::temp_dir()).is_some());
        assert!(available_space(&root.0.join("not").join("there")).is_some());
    }
}
