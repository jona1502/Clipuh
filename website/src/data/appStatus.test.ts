import { describe, expect, it } from "vitest";
import { elementStatus, isPlanned } from "./appStatus";

const ids = [
  "topbar.sourcePicker",
  "topbar.resolution",
  "topbar.fps",
  "topbar.replayLength",
  "topbar.hotkey",
  "topbar.replayToggle",
  "capture.changeSource",
  "capture.storageEstimate",
  "capture.codec",
  "capture.recentClips",
  "mixer.gameAudio",
  "mixer.desktopAudio",
  "mixer.microphone",
  "mixer.meters",
  "mixer.gain",
  "mixer.mute",
  "mixer.solo",
  "mixer.addSource",
  "library.grid",
  "library.listView",
  "library.favorites",
  "library.tabGameClips",
  "library.tabRecordings",
  "library.search",
  "library.filters",
  "library.sort",
  "library.multiSelect",
  "library.delete",
  "library.openFolder",
  "library.openInEditor",
  "editor.rename",
  "editor.playback",
  "editor.trim",
  "editor.multitrack",
  "editor.trackLock",
  "editor.trackVisibility",
  "editor.trackMute",
  "editor.split",
  "editor.deleteSegment",
  "editor.freeze",
  "editor.tabEffects",
  "editor.tabAudio",
  "editor.tabText",
  "editor.export",
  "editor.undoRedo",
  "editor.snap",
  "editor.zoom",
  "editor.addTrack",
  "settings.resolution",
  "settings.fps",
  "settings.encoder",
  "settings.bitrate",
  "settings.audioDevices",
  "settings.separateTracks",
  "settings.replayDuration",
  "settings.bufferLocation",
  "settings.autoStartReplay",
  "settings.hotkeySaveClip",
  "settings.hotkeyRecording",
  "settings.hotkeyOpenApp",
  "settings.clipLocation",
  "settings.retention",
  "settings.autoCleanup",
  "settings.theme",
  "settings.accent",
  "sidebar.storageMeter",
] as const;

describe("elementStatus", () => {
  it("has every mockup element id", () => {
    for (const id of ids) expect(elementStatus).toHaveProperty(id);
  });
  it("has no extra ids", () => {
    expect(Object.keys(elementStatus)).toHaveLength(ids.length);
  });
  it("uses only valid statuses", () => {
    for (const v of Object.values(elementStatus)) {
      expect(["planned", "in-development"]).toContain(v);
    }
  });
  it("isPlanned matches status", () => {
    for (const id of ids) {
      expect(isPlanned(id)).toBe(elementStatus[id] === "planned");
    }
  });
});
