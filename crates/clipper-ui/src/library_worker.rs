//! Shared, serialized index and poster worker for dashboard and library views.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clipper_media::{thumbnail::thumbnail, trim::probe};
use clipper_storage::library::{self, Clip};
use gpui::RenderImage;

const THUMBNAIL_WIDTH: u32 = 320;
const MAX_THUMBNAIL_HEIGHT: u32 = 320;
const MAX_CACHED_PREVIEWS: usize = 40;

pub(crate) struct LibraryWorker {
    pending: Arc<Mutex<Option<Request>>>,
    wake: async_channel::Sender<()>,
    events: async_channel::Sender<Event>,
    stopped: Arc<AtomicBool>,
}

pub(crate) struct Request {
    pub directory: PathBuf,
    pub refresh: bool,
    pub metadata_all: bool,
    pub wanted: Vec<String>,
    pub generation: u64,
}

pub(crate) enum Event {
    Index {
        generation: u64,
        clips: Vec<Clip>,
    },
    MetadataReady {
        generation: u64,
    },
    Preview {
        generation: u64,
        id: String,
        duration: Option<Duration>,
        image: Option<Arc<RenderImage>>,
    },
    Error {
        generation: u64,
        message: String,
    },
}

impl LibraryWorker {
    pub fn start() -> (Self, async_channel::Receiver<Event>) {
        let (events, receiver) = async_channel::bounded(4);
        let (wake, wake_receiver) = async_channel::bounded(1);
        let pending = Arc::new(Mutex::new(None));
        let stopped = Arc::new(AtomicBool::new(false));

        let thread_pending = Arc::clone(&pending);
        let thread_events = events.clone();
        let thread_stopped = Arc::clone(&stopped);
        std::thread::Builder::new()
            .name("clip-library".into())
            .spawn(move || run_worker(thread_pending, wake_receiver, thread_events, thread_stopped))
            .expect("could not start clip library worker");

        (
            Self {
                pending,
                wake,
                events,
                stopped,
            },
            receiver,
        )
    }

    pub fn request(&self, request: Request) {
        if self.stopped.load(Ordering::Acquire) {
            return;
        }
        coalesce_request(&mut self.pending.lock().unwrap(), request);
        // A full wake channel already guarantees that the worker will inspect
        // the latest coalesced request.
        let _ = self.wake.try_send(());
    }
}

impl Drop for LibraryWorker {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        // Closing both channels releases a worker blocked waiting for work or
        // blocked sending into a full event channel. We deliberately do not
        // join: dropping the UI owner must not wait for Media Foundation.
        self.wake.close();
        self.events.close();
    }
}

fn coalesce_request(pending: &mut Option<Request>, mut next: Request) {
    if let Some(previous) = pending.take() {
        next.refresh |= previous.refresh;
    }
    *pending = Some(next);
}

fn run_worker(
    pending: Arc<Mutex<Option<Request>>>,
    wake: async_channel::Receiver<()>,
    events: async_channel::Sender<Event>,
    stopped: Arc<AtomicBool>,
) {
    let mut state = WorkerState::default();

    loop {
        if wake.recv_blocking().is_err() || stopped.load(Ordering::Acquire) {
            return;
        }
        let request = pending.lock().unwrap().take();
        let Some(request) = request else { continue };
        if !process_request(request, &mut state, &pending, &events, &stopped) {
            return;
        }
    }
}

#[derive(Default)]
struct WorkerState {
    directory: Option<PathBuf>,
    clips: Vec<Clip>,
    cache: ThumbnailCache,
    probed_metadata: HashSet<CacheKey>,
    probed_dimensions: HashMap<CacheKey, (u32, u32)>,
    index_dirty: bool,
}

/// Returns false when the receiver has closed or the worker has been stopped.
fn process_request(
    request: Request,
    state: &mut WorkerState,
    pending: &Mutex<Option<Request>>,
    events: &async_channel::Sender<Event>,
    stopped: &AtomicBool,
) -> bool {
    let mut request = request;

    if state.directory.as_deref() != Some(request.directory.as_path()) {
        state.directory = Some(request.directory.clone());
        state.clips.clear();
        state.cache.clear();
        state.probed_metadata.clear();
        state.probed_dimensions.clear();
        state.index_dirty = false;
        request.refresh = true;
    }

    if is_stale(pending, stopped) {
        preserve_refresh_intent(pending, request.refresh);
        return true;
    }

    if request.refresh {
        match library::scan(&request.directory, &state.clips) {
            Ok(scanned) => {
                state.clips = scanned;
                state.cache.retain_for_clips(&state.clips);
                reconcile_probed_metadata(state);
                state.index_dirty = true;
            }
            Err(error) => {
                if is_stale(pending, stopped) {
                    preserve_refresh_intent(pending, true);
                    return !stopped.load(Ordering::Acquire);
                }
                return send_event(
                    events,
                    Event::Error {
                        generation: request.generation,
                        message: error.to_string(),
                    },
                    stopped,
                );
            }
        }
    }

    if state.index_dirty {
        if stopped.load(Ordering::Acquire) {
            return false;
        }
        let generation = {
            let newer = pending.lock().unwrap();
            index_event_generation(state.index_dirty, &request, newer.as_ref())
        };
        let Some(generation) = generation else {
            return true;
        };
        return send_index(
            generation,
            &state.clips,
            &mut state.index_dirty,
            pending,
            events,
            stopped,
        );
    }

    match probe_missing_metadata(state, &request, pending, stopped, |clip| {
        probe(&clip.path)
            .ok()
            .map(|info| (info.duration, info.width, info.height))
    }) {
        MetadataProgress::NotRequested => {}
        MetadataProgress::Cancelled { probed } => {
            state.index_dirty |= probed;
            return !stopped.load(Ordering::Acquire);
        }
        MetadataProgress::Complete { probed } => {
            state.index_dirty |= probed;
            if request.metadata_all {
                if probed {
                    if stopped.load(Ordering::Acquire) {
                        return false;
                    }
                    let generation = {
                        let newer = pending.lock().unwrap();
                        index_event_generation(state.index_dirty, &request, newer.as_ref())
                    };
                    let Some(generation) = generation else {
                        return true;
                    };
                    if !send_index(
                        generation,
                        &state.clips,
                        &mut state.index_dirty,
                        pending,
                        events,
                        stopped,
                    ) {
                        return false;
                    }
                }
                if stopped.load(Ordering::Acquire) {
                    return false;
                }
                if !send_event(
                    events,
                    Event::MetadataReady {
                        generation: request.generation,
                    },
                    stopped,
                ) {
                    return false;
                }
                if probed {
                    // The index event causes the UI to request its current
                    // wanted previews again at the same generation.
                    return true;
                }
            }
        }
    }

    let mut sent = HashSet::new();
    for id in request.wanted {
        if !sent.insert(id.clone()) {
            continue;
        }
        if is_stale(pending, stopped) {
            break;
        }
        let Some(index) = state.clips.iter().position(|clip| clip.id == id) else {
            continue;
        };
        let clip = state.clips[index].clone();
        let key = CacheKey::from_clip(&clip);

        let preview = if let Some(preview) = state.cache.get(&key) {
            preview
        } else {
            if is_stale(pending, stopped) {
                break;
            }
            // Probe once for cache misses. Its dimensions let us skip a poster
            // whose scaled height would produce an extreme allocation.
            let dimensions = if state.probed_metadata.contains(&key) {
                state.probed_dimensions.get(&key).copied()
            } else {
                let info = probe(&clip.path).ok();
                state.probed_metadata.insert(key.clone());
                if let Some(info) = info {
                    state.clips[index].duration = Some(info.duration);
                    let dimensions = (info.width, info.height);
                    state.probed_dimensions.insert(key.clone(), dimensions);
                    Some(dimensions)
                } else {
                    None
                }
            };
            let duration = state.clips[index].duration;

            if is_stale(pending, stopped) {
                break;
            }
            let image = dimensions
                .and_then(|(width, height)| thumbnail_width(width, height))
                .and_then(|width| thumbnail(&clip.path, width).ok())
                .and_then(render_image);
            let preview = CachedPreview { duration, image };
            state.cache.insert(key, preview.clone());
            preview
        };

        if is_stale(pending, stopped) {
            break;
        }
        if !send_event(
            events,
            Event::Preview {
                generation: request.generation,
                id,
                duration: preview.duration,
                image: preview.image,
            },
            stopped,
        ) {
            return false;
        }
    }
    !stopped.load(Ordering::Acquire)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MetadataProgress {
    NotRequested,
    Complete { probed: bool },
    Cancelled { probed: bool },
}

fn probe_missing_metadata(
    state: &mut WorkerState,
    request: &Request,
    pending: &Mutex<Option<Request>>,
    stopped: &AtomicBool,
    mut probe_clip: impl FnMut(&Clip) -> Option<(Duration, u32, u32)>,
) -> MetadataProgress {
    if !request.metadata_all {
        return MetadataProgress::NotRequested;
    }

    let mut probed = false;
    for index in 0..state.clips.len() {
        let clip = state.clips[index].clone();
        let key = CacheKey::from_clip(&clip);
        if let Some(_duration) = clip.duration {
            state.probed_metadata.insert(key);
            continue;
        }
        if state.probed_metadata.contains(&key) {
            continue;
        }
        if is_stale(pending, stopped) {
            return MetadataProgress::Cancelled { probed };
        }

        let info = probe_clip(&clip);
        state.probed_metadata.insert(key.clone());
        if let Some((duration, width, height)) = info {
            state.clips[index].duration = Some(duration);
            state.probed_dimensions.insert(key, (width, height));
        }
        // Failed probes are remembered too, so corrupt clips don't retry on
        // every refresh or page change with the same file fingerprint.
        probed = true;
    }

    if is_stale(pending, stopped) {
        MetadataProgress::Cancelled { probed }
    } else {
        MetadataProgress::Complete { probed }
    }
}

fn reconcile_probed_metadata(state: &mut WorkerState) {
    let current: HashSet<_> = state.clips.iter().map(CacheKey::from_clip).collect();
    state.probed_metadata.retain(|key| current.contains(key));
    state
        .probed_dimensions
        .retain(|key, _| current.contains(key));
    for clip in &state.clips {
        if clip.duration.is_some() {
            state.probed_metadata.insert(CacheKey::from_clip(clip));
        }
    }
}

fn is_stale(pending: &Mutex<Option<Request>>, stopped: &AtomicBool) -> bool {
    stopped.load(Ordering::Acquire) || pending.lock().unwrap().is_some()
}

fn preserve_refresh_intent(pending: &Mutex<Option<Request>>, refresh: bool) {
    if refresh && let Some(request) = pending.lock().unwrap().as_mut() {
        request.refresh = true;
    }
}

fn index_event_generation(
    index_dirty: bool,
    request: &Request,
    pending: Option<&Request>,
) -> Option<u64> {
    if pending.is_some() {
        None
    } else if index_dirty || request.refresh {
        Some(request.generation)
    } else {
        None
    }
}

fn send_index(
    generation: u64,
    clips: &[Clip],
    index_dirty: &mut bool,
    pending: &Mutex<Option<Request>>,
    events: &async_channel::Sender<Event>,
    stopped: &AtomicBool,
) -> bool {
    *index_dirty = true;
    if !send_event(
        events,
        Event::Index {
            generation,
            clips: clips.to_vec(),
        },
        stopped,
    ) {
        return false;
    }
    // Keep the snapshot dirty if a newer request arrived while sending. It
    // will receive the same index at its generation before any previews.
    let pending = pending.lock().unwrap();
    finish_index_send(index_dirty, generation, pending.as_ref());
    true
}

fn finish_index_send(index_dirty: &mut bool, sent_generation: u64, pending: Option<&Request>) {
    *index_dirty =
        pending.is_some_and(|request| request.refresh || request.generation != sent_generation);
}

fn send_event(events: &async_channel::Sender<Event>, event: Event, stopped: &AtomicBool) -> bool {
    if stopped.load(Ordering::Acquire) {
        return false;
    }
    events.send_blocking(event).is_ok()
}

/// Chooses a decode width that keeps both dimensions bounded. The media
/// helper scales only by width, so very tall clips need a smaller width.
fn thumbnail_width(width: u32, height: u32) -> Option<u32> {
    if width == 0 || height == 0 {
        return None;
    }
    let height_limited_width =
        u64::from(width) * u64::from(MAX_THUMBNAIL_HEIGHT) / u64::from(height);
    if height_limited_width == 0 {
        return None;
    }
    let height_limited_width = height_limited_width.min(u64::from(THUMBNAIL_WIDTH)) as u32;
    Some(width.min(THUMBNAIL_WIDTH).min(height_limited_width).max(1))
}

fn render_image(thumbnail: clipper_media::thumbnail::Thumbnail) -> Option<Arc<RenderImage>> {
    if thumbnail.width == 0 || thumbnail.height == 0 || thumbnail.height > MAX_THUMBNAIL_HEIGHT {
        return None;
    }
    let expected_len = (thumbnail.width as usize)
        .checked_mul(thumbnail.height as usize)?
        .checked_mul(4)?;
    if expected_len > MAX_THUMBNAIL_WIDTH_BYTES || thumbnail.bgra.len() != expected_len {
        return None;
    }
    let buffer = image::RgbaImage::from_raw(thumbnail.width, thumbnail.height, thumbnail.bgra)?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(buffer)])))
}

const MAX_THUMBNAIL_WIDTH_BYTES: usize =
    THUMBNAIL_WIDTH as usize * MAX_THUMBNAIL_HEIGHT as usize * 4;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CacheKey {
    id: String,
    size_bytes: u64,
    modified: std::time::SystemTime,
}

impl CacheKey {
    fn from_clip(clip: &Clip) -> Self {
        Self {
            id: clip.id.clone(),
            size_bytes: clip.size_bytes,
            modified: clip.modified,
        }
    }
}

#[derive(Clone)]
struct CachedPreview {
    duration: Option<Duration>,
    image: Option<Arc<RenderImage>>,
}

#[derive(Default)]
struct ThumbnailCache {
    entries: HashMap<CacheKey, CachedPreview>,
    order: VecDeque<CacheKey>,
}

impl ThumbnailCache {
    fn get(&mut self, key: &CacheKey) -> Option<CachedPreview> {
        let preview = self.entries.get(key)?.clone();
        if let Some(index) = self.order.iter().position(|entry| entry == key) {
            self.order.remove(index);
        }
        self.order.push_back(key.clone());
        Some(preview)
    }

    fn insert(&mut self, key: CacheKey, preview: CachedPreview) {
        if let Some(index) = self.order.iter().position(|entry| entry == &key) {
            self.order.remove(index);
        }
        self.entries.insert(key.clone(), preview);
        self.order.push_back(key);
        while self.entries.len() > MAX_CACHED_PREVIEWS {
            if let Some(expired) = self.order.pop_front() {
                self.entries.remove(&expired);
            }
        }
    }

    fn retain_for_clips(&mut self, clips: &[Clip]) {
        let current: HashMap<_, _> = clips
            .iter()
            .map(|clip| (clip.id.clone(), CacheKey::from_clip(clip)))
            .collect();
        self.entries
            .retain(|key, _| current.get(&key.id) == Some(key));
        self.order.retain(|key| self.entries.contains_key(key));
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    #[cfg(test)]
    fn contains(&self, key: &CacheKey) -> bool {
        self.entries.contains_key(key)
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn clip(id: &str, size_bytes: u64, modified: SystemTime) -> Clip {
        Clip {
            id: id.to_owned(),
            path: PathBuf::from(format!("{id}.mp4")),
            name: id.to_owned(),
            source: None,
            duration: None,
            size_bytes,
            created: None,
            modified,
        }
    }

    #[test]
    fn cache_invalidation_tracks_asset_metadata_and_removal() {
        let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
        let original = clip("a", 100, modified);
        let original_key = CacheKey::from_clip(&original);
        let mut cache = ThumbnailCache::default();
        cache.insert(
            original_key.clone(),
            CachedPreview {
                duration: None,
                image: None,
            },
        );
        cache.retain_for_clips(std::slice::from_ref(&original));
        assert!(cache.contains(&original_key));

        let replacement = clip("a", 101, modified);
        cache.retain_for_clips(std::slice::from_ref(&replacement));
        assert!(!cache.contains(&original_key));

        cache.insert(
            CacheKey::from_clip(&replacement),
            CachedPreview {
                duration: None,
                image: None,
            },
        );
        let modified_replacement = clip("a", 101, modified + Duration::from_secs(1));
        cache.retain_for_clips(std::slice::from_ref(&modified_replacement));
        assert!(cache.is_empty());

        cache.insert(
            CacheKey::from_clip(&modified_replacement),
            CachedPreview {
                duration: None,
                image: None,
            },
        );
        cache.retain_for_clips(&[]);
        assert!(cache.is_empty());
    }

    #[test]
    fn pending_requests_keep_latest_generation_and_wanted_ids_but_or_refresh() {
        let mut pending = Some(Request {
            directory: PathBuf::from("old"),
            refresh: true,
            metadata_all: true,
            wanted: vec!["old-id".into()],
            generation: 2,
        });
        coalesce_request(
            &mut pending,
            Request {
                directory: PathBuf::from("new"),
                refresh: false,
                metadata_all: false,
                wanted: vec!["new-id".into()],
                generation: 3,
            },
        );

        let request = pending.unwrap();
        assert_eq!(request.directory, PathBuf::from("new"));
        assert_eq!(request.generation, 3);
        assert_eq!(request.wanted, ["new-id"]);
        assert!(request.refresh);
        assert!(!request.metadata_all);
    }

    #[test]
    fn superseding_wanted_request_receives_dirty_index_without_rescanning() {
        let first = Request {
            directory: PathBuf::from("clips"),
            refresh: true,
            metadata_all: false,
            wanted: vec![],
            generation: 8,
        };
        let newer = Request {
            directory: PathBuf::from("clips"),
            refresh: false,
            metadata_all: false,
            wanted: vec!["new-id".into()],
            generation: 9,
        };
        let mut dirty = true;
        let pending = Mutex::new(Some(newer));

        assert_eq!(
            index_event_generation(dirty, &first, pending.lock().unwrap().as_ref()),
            None
        );
        let latest = pending.lock().unwrap().take().unwrap();
        assert_eq!(index_event_generation(dirty, &latest, None), Some(9));

        finish_index_send(&mut dirty, 9, None);
        assert!(!dirty);
        let previews = Request {
            directory: PathBuf::from("clips"),
            refresh: false,
            metadata_all: false,
            wanted: vec!["new-id".into()],
            generation: 10,
        };
        assert_eq!(index_event_generation(dirty, &previews, None), None);
    }

    #[test]
    fn a_request_that_cancels_an_unstarted_refresh_inherits_refresh_intent() {
        let pending = Mutex::new(Some(Request {
            directory: PathBuf::from("clips"),
            refresh: false,
            metadata_all: false,
            wanted: vec!["latest".into()],
            generation: 4,
        }));

        preserve_refresh_intent(&pending, true);

        let request = pending.lock().unwrap().take().unwrap();
        assert_eq!(request.generation, 4);
        assert_eq!(request.wanted, ["latest"]);
        assert!(request.refresh);
    }

    #[test]
    fn same_generation_wanted_request_acknowledges_sent_index() {
        let mut dirty = true;
        let acknowledgement = Request {
            directory: PathBuf::from("clips"),
            refresh: false,
            metadata_all: false,
            wanted: vec!["clip-id".into()],
            generation: 2,
        };
        finish_index_send(&mut dirty, 2, Some(&acknowledgement));
        assert!(!dirty);

        let newer = Request {
            directory: PathBuf::from("clips"),
            refresh: false,
            metadata_all: false,
            wanted: vec!["clip-id".into()],
            generation: 3,
        };
        finish_index_send(&mut dirty, 2, Some(&newer));
        assert!(dirty);

        let refresh = Request {
            directory: PathBuf::from("clips"),
            refresh: true,
            metadata_all: false,
            wanted: vec!["clip-id".into()],
            generation: 2,
        };
        finish_index_send(&mut dirty, 2, Some(&refresh));
        assert!(dirty);
    }

    #[test]
    fn metadata_all_flag_uses_the_latest_coalesced_request() {
        let mut pending = Some(Request {
            directory: PathBuf::from("clips"),
            refresh: false,
            metadata_all: true,
            wanted: vec!["a".into()],
            generation: 2,
        });
        coalesce_request(
            &mut pending,
            Request {
                directory: PathBuf::from("clips"),
                refresh: false,
                metadata_all: false,
                wanted: vec!["b".into()],
                generation: 3,
            },
        );

        let request = pending.unwrap();
        assert_eq!(request.generation, 3);
        assert_eq!(request.wanted, ["b"]);
        assert!(!request.metadata_all);
    }

    #[test]
    fn cache_evicts_oldest_preview_at_the_forty_entry_limit() {
        let mut cache = ThumbnailCache::default();
        let mut keys = Vec::new();
        for index in 0..=MAX_CACHED_PREVIEWS {
            let clip = clip(
                &format!("clip-{index}"),
                index as u64,
                SystemTime::UNIX_EPOCH + Duration::from_secs(index as u64),
            );
            let key = CacheKey::from_clip(&clip);
            cache.insert(
                key.clone(),
                CachedPreview {
                    duration: None,
                    image: None,
                },
            );
            keys.push(key);
        }

        assert_eq!(cache.len(), MAX_CACHED_PREVIEWS);
        assert!(!cache.contains(&keys[0]));
        assert!(cache.contains(keys.last().unwrap()));
    }

    #[test]
    fn thumbnail_decode_width_caps_extreme_portrait_height() {
        assert_eq!(thumbnail_width(1_920, 1_080), Some(320));
        assert_eq!(thumbnail_width(1_080, 1_920), Some(180));
        assert_eq!(thumbnail_width(1_000, 100_000), Some(3));
        assert_eq!(thumbnail_width(1_000, 1_000_000), None);
        assert_eq!(thumbnail_width(1, 5_000), None);
        assert_eq!(thumbnail_width(0, 10), None);
    }

    #[test]
    fn metadata_probe_failures_are_cached_for_the_asset_fingerprint() {
        let clip = clip("broken", 0, SystemTime::UNIX_EPOCH);
        let mut state = WorkerState {
            clips: vec![clip],
            ..WorkerState::default()
        };
        let request = Request {
            directory: PathBuf::from("clips"),
            refresh: false,
            metadata_all: true,
            wanted: vec![],
            generation: 1,
        };
        let pending = Mutex::new(None);
        let stopped = AtomicBool::new(false);
        let key = CacheKey::from_clip(&state.clips[0]);

        assert_eq!(
            probe_missing_metadata(&mut state, &request, &pending, &stopped, |_| None),
            MetadataProgress::Complete { probed: true }
        );
        assert!(state.probed_metadata.contains(&key));
        assert_eq!(
            probe_missing_metadata(&mut state, &request, &pending, &stopped, |_| {
                panic!("cached failure must not probe again")
            }),
            MetadataProgress::Complete { probed: false }
        );
    }

    #[test]
    fn newer_metadata_all_false_request_cancels_bulk_probes_between_files() {
        let mut state = WorkerState {
            clips: (0..3)
                .map(|index| {
                    clip(
                        &format!("clip-{index}"),
                        index,
                        SystemTime::UNIX_EPOCH + Duration::from_secs(index),
                    )
                })
                .collect(),
            ..WorkerState::default()
        };
        let request = Request {
            directory: PathBuf::from("clips"),
            refresh: false,
            metadata_all: true,
            wanted: vec![],
            generation: 1,
        };
        let pending = Mutex::new(None);
        let stopped = AtomicBool::new(false);
        let mut probes = 0;

        let progress = probe_missing_metadata(&mut state, &request, &pending, &stopped, |_| {
            probes += 1;
            if probes == 1 {
                coalesce_request(
                    &mut pending.lock().unwrap(),
                    Request {
                        directory: PathBuf::from("clips"),
                        refresh: false,
                        metadata_all: false,
                        wanted: vec!["clip-0".into()],
                        generation: 2,
                    },
                );
            }
            Some((Duration::from_secs(2), 320, 240))
        });

        assert_eq!(progress, MetadataProgress::Cancelled { probed: true });
        assert_eq!(probes, 1);
        assert_eq!(state.clips[0].duration, Some(Duration::from_secs(2)));
        assert_eq!(state.clips[1].duration, None);
        assert_eq!(state.probed_metadata.len(), 1);
        assert!(!pending.lock().unwrap().as_ref().unwrap().metadata_all);
    }

    #[test]
    fn worker_refreshes_previews_and_observes_removal() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "clip-library-worker-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("a.mp4"), []).unwrap();

        let (worker, events) = LibraryWorker::start();
        worker.request(Request {
            directory: directory.clone(),
            refresh: true,
            metadata_all: false,
            wanted: vec![],
            generation: 1,
        });
        let clip_id = match receive_before(&events, Duration::from_secs(3)) {
            Event::Index {
                generation: 1,
                clips,
            } => {
                assert_eq!(clips.len(), 1);
                clips[0].id.clone()
            }
            Event::Error { message, .. } => panic!("scan failed: {message}"),
            _ => panic!("expected the first index event"),
        };

        worker.request(Request {
            directory: directory.clone(),
            refresh: false,
            metadata_all: false,
            wanted: vec![clip_id.clone()],
            generation: 1,
        });
        match receive_before(&events, Duration::from_secs(3)) {
            Event::Preview {
                generation: 1,
                id,
                duration: None,
                image: None,
            } => assert_eq!(id, clip_id),
            Event::Error { message, .. } => panic!("preview request failed: {message}"),
            _ => panic!("expected one preview event, not a repeated index"),
        }

        std::fs::remove_file(directory.join("a.mp4")).unwrap();
        worker.request(Request {
            directory: directory.clone(),
            refresh: true,
            metadata_all: false,
            wanted: vec![],
            generation: 3,
        });
        match receive_before(&events, Duration::from_secs(3)) {
            Event::Index {
                generation: 3,
                clips,
            } => assert!(clips.is_empty()),
            Event::Error { message, .. } => panic!("removal scan failed: {message}"),
            _ => panic!("expected an empty index after removal"),
        }

        drop(worker);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn metadata_all_probes_all_clips_then_reuses_failures_for_wanted_preview() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "clip-library-metadata-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        for name in ["a.mp4", "b.mp4", "c.mp4"] {
            std::fs::write(directory.join(name), []).unwrap();
        }

        let (worker, events) = LibraryWorker::start();
        worker.request(Request {
            directory: directory.clone(),
            refresh: true,
            metadata_all: true,
            wanted: vec![],
            generation: 10,
        });
        let clip_id = match receive_before(&events, Duration::from_secs(3)) {
            Event::Index {
                generation: 10,
                clips,
            } => {
                assert_eq!(clips.len(), 3);
                clips[0].id.clone()
            }
            Event::Error { message, .. } => panic!("scan failed: {message}"),
            _ => panic!("expected initial index"),
        };

        worker.request(Request {
            directory: directory.clone(),
            refresh: false,
            metadata_all: true,
            wanted: vec![clip_id.clone()],
            generation: 10,
        });
        match receive_before(&events, Duration::from_secs(3)) {
            Event::Index {
                generation: 10,
                clips,
            } => {
                assert_eq!(clips.len(), 3);
                assert!(clips.iter().all(|clip| clip.duration.is_none()));
                // Same-generation acknowledgement mirrors the UI after Index.
                worker.request(Request {
                    directory: directory.clone(),
                    refresh: false,
                    metadata_all: true,
                    wanted: vec![clip_id.clone()],
                    generation: 10,
                });
            }
            Event::Error { message, .. } => panic!("metadata request failed: {message}"),
            _ => panic!("expected completed metadata index"),
        }
        assert!(matches!(
            receive_before(&events, Duration::from_secs(3)),
            Event::MetadataReady { generation: 10 }
        ));
        assert!(matches!(
            receive_before(&events, Duration::from_secs(3)),
            Event::MetadataReady { generation: 10 }
        ));
        match receive_before(&events, Duration::from_secs(3)) {
            Event::Preview {
                generation: 10,
                id,
                duration: None,
                image: None,
            } => assert_eq!(id, clip_id),
            _ => panic!("expected wanted preview without another index"),
        }

        drop(worker);
        let _ = std::fs::remove_dir_all(directory);
    }

    fn receive_before(events: &async_channel::Receiver<Event>, timeout: Duration) -> Event {
        use std::time::Instant;

        let deadline = Instant::now() + timeout;
        loop {
            match events.try_recv() {
                Ok(event) => return event,
                Err(async_channel::TryRecvError::Closed) => panic!("worker event channel closed"),
                Err(async_channel::TryRecvError::Empty) if Instant::now() >= deadline => {
                    panic!("timed out waiting for worker event")
                }
                Err(async_channel::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }
}
