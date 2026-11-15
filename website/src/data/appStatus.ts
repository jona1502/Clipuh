/**
 * Truth table of the mockup UI elements used by the app tour (audit of
 * crates/*). Status is truthful to the desktop app:
 * - "in-development": the element exists in the current development build.
 * - "planned": not implemented; the tour must render a "Planned" badge.
 * Each entry's comment names the evidence (file path) or "not found".
 * Nothing is "released": no public build exists.
 */
import type { FeatureStatus } from "./features";

export type ElementId =
  | "topbar.sourcePicker"
  | "topbar.resolution"
  | "topbar.fps"
  | "topbar.replayLength"
  | "topbar.hotkey"
  | "topbar.replayToggle"
  | "capture.changeSource"
  | "capture.storageEstimate"
  | "capture.codec"
  | "capture.recentClips"
  | "mixer.gameAudio"
  | "mixer.desktopAudio"
  | "mixer.microphone"
  | "mixer.meters"
  | "mixer.gain"
  | "mixer.mute"
  | "mixer.solo"
  | "mixer.addSource"
  | "library.grid"
  | "library.listView"
  | "library.favorites"
  | "library.tabGameClips"
  | "library.tabRecordings"
  | "library.search"
  | "library.filters"
  | "library.sort"
  | "library.multiSelect"
  | "library.delete"
  | "library.openFolder"
  | "library.openInEditor"
  | "editor.rename"
  | "editor.playback"
  | "editor.trim"
  | "editor.multitrack"
  | "editor.trackLock"
  | "editor.trackVisibility"
  | "editor.trackMute"
  | "editor.split"
  | "editor.deleteSegment"
  | "editor.freeze"
  | "editor.tabEffects"
  | "editor.tabAudio"
  | "editor.tabText"
  | "editor.export"
  | "editor.undoRedo"
  | "editor.snap"
  | "editor.zoom"
  | "editor.addTrack"
  | "editor.linkTracks"
  | "settings.resolution"
  | "settings.fps"
  | "settings.encoder"
  | "settings.bitrate"
  | "settings.audioDevices"
  | "settings.separateTracks"
  | "settings.replayDuration"
  | "settings.bufferLocation"
  | "settings.autoStartReplay"
  | "settings.hotkeySaveClip"
  | "settings.hotkeyRecording"
  | "settings.hotkeyOpenApp"
  | "settings.clipLocation"
  | "settings.retention"
  | "settings.autoCleanup"
  | "settings.theme"
  | "settings.accent"
  | "sidebar.storageMeter";

export const elementStatus: Record<ElementId, FeatureStatus> = {
  // crates/clipper-ui/src/views/capture_bar.rs (bar_source)
  "topbar.sourcePicker": "in-development",
  // crates/clipper-ui/src/views/capture_bar.rs (CaptureMenu::Resolution)
  "topbar.resolution": "in-development",
  // crates/clipper-ui/src/views/capture_bar.rs (CaptureMenu::FrameRate)
  "topbar.fps": "in-development",
  // crates/clipper-ui/src/views/capture_bar.rs (CaptureMenu::ReplayDuration)
  "topbar.replayLength": "in-development",
  // crates/clipper-ui/src/views/capture_bar.rs (hotkey_bar_control)
  "topbar.hotkey": "in-development",
  // crates/clipper-ui/src/views/capture_bar.rs (bar_replay)
  "topbar.replayToggle": "in-development",
  // crates/clipper-ui/src/views/source_card.rs (Change Source)
  "capture.changeSource": "in-development",
  // crates/clipper-ui/src/views/storage_card.rs, crates/clipper-storage/src/storage_projection.rs
  "capture.storageEstimate": "in-development",
  // crates/clipper-ui/src/views/storage_card.rs (codec_label, H.264)
  "capture.codec": "in-development",
  // crates/clipper-ui/src/views/recent_card.rs
  "capture.recentClips": "in-development",
  // crates/clipper-ui/src/views/audio.rs (AudioSourceType::Game)
  "mixer.gameAudio": "in-development",
  // crates/clipper-ui/src/views/audio.rs (AudioSourceType::Desktop)
  "mixer.desktopAudio": "in-development",
  // crates/clipper-ui/src/views/audio.rs (AudioSourceType::Microphone)
  "mixer.microphone": "in-development",
  // crates/clipper-ui/src/meter.rs, crates/clipper-ui/src/views/audio.rs (level meters)
  "mixer.meters": "in-development",
  // crates/clipper-ui/src/views/audio.rs (audio_set_gain)
  "mixer.gain": "in-development",
  // crates/clipper-ui/src/views/audio.rs (Mute)
  "mixer.mute": "in-development",
  // crates/clipper-ui/src/views/audio.rs (Solo)
  "mixer.solo": "in-development",
  // crates/clipper-ui/src/views/audio.rs (Add Source)
  "mixer.addSource": "in-development",
  // crates/clipper-storage/src/config.rs (LibraryView::Grid)
  "library.grid": "in-development",
  // crates/clipper-storage/src/config.rs (LibraryView::List)
  "library.listView": "in-development",
  // crates/clipper-ui/src/views/clips.rs (Tab::Favorites)
  "library.favorites": "in-development",
  // crates/clipper-ui/src/views/clips.rs (Tab::GameClips)
  "library.tabGameClips": "in-development",
  // crates/clipper-ui/src/views/clips.rs (Tab::Recordings)
  "library.tabRecordings": "in-development",
  // crates/clipper-ui/src/views/clips.rs (search input)
  "library.search": "in-development",
  // crates/clipper-ui/src/views/clips.rs (Menu::Source filter)
  "library.filters": "in-development",
  // crates/clipper-ui/src/views/clips.rs (SORTS, Menu::Sort)
  "library.sort": "in-development",
  // crates/clipper-ui/src/views/clips.rs (SelectAll, selected())
  "library.multiSelect": "in-development",
  // crates/clipper-ui/src/views/clips.rs (confirm_delete)
  "library.delete": "in-development",
  // crates/clipper-ui/src/views/clips.rs (shell::reveal_in_folder)
  "library.openFolder": "in-development",
  // crates/clipper-ui/src/views/clips.rs (Edit action)
  "library.openInEditor": "in-development",
  // not found in crates/clipper-ui/src/views/editor*; rename exists only in the library
  "editor.rename": "planned",
  // crates/clipper-ui/src/views/editor.rs (toggle_playback)
  "editor.playback": "in-development",
  // crates/clipper-ui/src/views/editor/presentation.rs (Trim tab)
  "editor.trim": "in-development",
  // crates/clipper-editor/src/audio.rs (several audio tracks), crates/clipper-ui/src/views/editor/audio.rs
  "editor.multitrack": "in-development",
  // not found in crates/clipper-editor or crates/clipper-ui/src/views/editor
  "editor.trackLock": "planned",
  // crates/clipper-ui/src/views/editor/audio.rs (audio_toggle, track enabled)
  "editor.trackVisibility": "in-development",
  // crates/clipper-editor/src/audio.rs (set_track_mute)
  "editor.trackMute": "in-development",
  // crates/clipper-ui/src/views/editor/presentation.rs (Split)
  "editor.split": "in-development",
  // crates/clipper-ui/src/views/editor/presentation.rs (Delete)
  "editor.deleteSegment": "in-development",
  // not found in crates/clipper-editor or crates/clipper-ui/src/views/editor
  "editor.freeze": "planned",
  // crates/clipper-ui/src/views/editor/presentation.rs ("Coming later")
  "editor.tabEffects": "planned",
  // crates/clipper-ui/src/views/editor/audio.rs (audio_inspector)
  "editor.tabAudio": "in-development",
  // crates/clipper-ui/src/views/editor/presentation.rs ("Coming later")
  "editor.tabText": "planned",
  // crates/clipper-ui/src/views/editor/presentation.rs (Export tab)
  "editor.export": "in-development",
  // crates/clipper-ui/src/views/editor/presentation.rs (Undo, Redo)
  "editor.undoRedo": "in-development",
  // not found (editor/playhead.rs SNAP is playhead smoothing, not timeline snapping)
  "editor.snap": "planned",
  // crates/clipper-ui/src/views/editor.rs (set_zoom)
  "editor.zoom": "in-development",
  // not found in crates/clipper-editor or crates/clipper-ui/src/views/editor
  "editor.addTrack": "planned",
  // not found in crates/clipper-ui/src/views/editor*: no link toggle; the audio
  // inspector only notes that video ranges stay linked
  "editor.linkTracks": "planned",
  // crates/clipper-ui/src/views/settings.rs (Resolution)
  "settings.resolution": "in-development",
  // crates/clipper-ui/src/views/settings.rs (Frame Rate)
  "settings.fps": "in-development",
  // crates/clipper-ui/src/views/settings.rs (Encoder)
  "settings.encoder": "in-development",
  // crates/clipper-ui/src/views/settings.rs (Bitrate)
  "settings.bitrate": "in-development",
  // crates/clipper-ui/src/views/settings.rs (audio sources), device choice in crates/clipper-ui/src/views/audio.rs
  "settings.audioDevices": "in-development",
  // crates/clipper-ui/src/views/settings.rs (Separate audio tracks)
  "settings.separateTracks": "in-development",
  // crates/clipper-ui/src/views/settings.rs (Replay Duration)
  "settings.replayDuration": "in-development",
  // crates/clipper-ui/src/views/settings.rs (Cache Folder)
  "settings.bufferLocation": "in-development",
  // crates/clipper-ui/src/views/settings.rs (Start replay automatically)
  "settings.autoStartReplay": "in-development",
  // crates/clipper-storage/src/config.rs (replay.hotkey), crates/clipper-ui/src/views/settings.rs
  "settings.hotkeySaveClip": "in-development",
  // not found: config has only replay.hotkey
  "settings.hotkeyRecording": "planned",
  // not found: config has only replay.hotkey
  "settings.hotkeyOpenApp": "planned",
  // crates/clipper-ui/src/views/settings.rs (Clip Location)
  "settings.clipLocation": "in-development",
  // not found in crates/clipper-storage/src/config.rs
  "settings.retention": "planned",
  // not found in crates/clipper-storage/src/config.rs
  "settings.autoCleanup": "planned",
  // crates/clipper-ui/src/views/settings.rs (ThemeMode)
  "settings.theme": "in-development",
  // crates/clipper-ui/src/views/settings.rs (Accent Color)
  "settings.accent": "in-development",
  // crates/clipper-ui/src/storage_status.rs, crates/clipper-ui/src/views/mod.rs (sidebar storage)
  "sidebar.storageMeter": "in-development",
};

export function isPlanned(id: ElementId): boolean {
  return elementStatus[id] === "planned";
}
