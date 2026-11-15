import { useSyncExternalStore } from "react";
import { INITIAL_CLIPS } from "./mockData";

export type PageId = "capture" | "library" | "editor" | "settings";
export type Resolution = "720p" | "1080p" | "1440p" | "2160p";
export type Fps = 30 | 60 | 120 | 144;
export type ReplaySec = 15 | 30 | 60 | 120 | 300;
export type Codec = "h264" | "hevc" | "av1";
export type HotkeyAction = "saveClip" | "recording" | "openApp";
export type AccentId =
  "violet" | "blue" | "cyan" | "green" | "yellow" | "orange" | "pink";

export interface Clip {
  id: string;
  title: string;
  gameId: string;
  kind: "game" | "recording";
  minutesAgo: number;
  durationSec: number;
  sizeMb: number;
  favorite: boolean;
}
export interface AudioSource {
  id: string;
  kind: "game" | "desktop" | "mic" | "extra";
  label: string;
  detail: string;
  gainDb: number;
  muted: boolean;
  solo: boolean;
}
export interface CaptureState {
  sourceId: string;
  resolution: Resolution;
  fps: Fps;
  replaySec: ReplaySec;
  codec: Codec;
  replayActive: boolean;
}
export interface SettingsState {
  theme: "dark" | "light" | "system";
  accent: AccentId;
  bitrateMbps: number;
  encoder: string;
  bufferLocation: "ram" | "disk" | "hybrid";
  autoStartReplay: boolean;
  separateTracks: boolean;
  clipLocation: string;
  keepDays: 7 | 14 | 30 | 90 | 0;
  autoCleanup: boolean;
  hotkeys: Record<HotkeyAction, string>;
  devices: { game: string; desktop: string; mic: string };
}
export interface AppState {
  capture: CaptureState;
  settings: SettingsState;
  clips: Clip[];
  sources: AudioSource[];
  editorClipId: string | null;
  storage: { usedGb: number; totalGb: number };
  lastError: string | null;
}

export type Action =
  | { type: "capture/set"; patch: Partial<CaptureState> }
  | { type: "settings/set"; patch: Partial<SettingsState> }
  | { type: "hotkey/set"; action: HotkeyAction; combo: string }
  | { type: "clip/toggleFavorite"; id: string }
  | { type: "clips/delete"; ids: string[] }
  | { type: "clips/restore"; clips: Clip[] }
  | { type: "clip/rename"; id: string; title: string }
  | { type: "editor/open"; id: string }
  | {
      type: "source/set";
      id: string;
      patch: Partial<Pick<AudioSource, "gainDb" | "muted" | "solo">>;
    }
  | { type: "source/add" }
  | { type: "error/clear" };

/** tokenVar names the CSS custom property defined by the app theme styles. */
export const ACCENTS: Record<AccentId, { label: string; tokenVar: string }> = {
  violet: { label: "Violet", tokenVar: "--app-accent-violet" },
  blue: { label: "Blue", tokenVar: "--app-accent-blue" },
  cyan: { label: "Cyan", tokenVar: "--app-accent-cyan" },
  green: { label: "Green", tokenVar: "--app-accent-green" },
  yellow: { label: "Yellow", tokenVar: "--app-accent-yellow" },
  orange: { label: "Orange", tokenVar: "--app-accent-orange" },
  pink: { label: "Pink", tokenVar: "--app-accent-pink" },
};

export function estimateClipMb(replaySec: number, bitrateMbps: number): number {
  return Math.round((bitrateMbps * replaySec) / 8);
}

const byNewest = (a: Clip, b: Clip) => a.minutesAgo - b.minutesAgo;

const sortedCache = new WeakMap<Clip[], Clip[]>();
/** Newest first. Memoized per clips array so selectors stay referentially stable. */
export function visibleClips(state: AppState): Clip[] {
  let s = sortedCache.get(state.clips);
  if (!s) {
    s = [...state.clips].sort(byNewest);
    sortedCache.set(state.clips, s);
  }
  return s;
}

export function createInitialState(): AppState {
  const clips = INITIAL_CLIPS.map((c) => ({ ...c }));
  return {
    capture: {
      sourceId: "ridge",
      resolution: "1080p",
      fps: 60,
      replaySec: 60,
      codec: "h264",
      replayActive: true,
    },
    settings: {
      theme: "dark",
      accent: "violet",
      bitrateMbps: 20,
      encoder: "Hardware (GPU)",
      bufferLocation: "ram",
      autoStartReplay: true,
      separateTracks: true,
      clipLocation: "C:\\Users\\You\\Videos\\clipuh",
      keepDays: 30,
      autoCleanup: false,
      hotkeys: {
        saveClip: "Alt + S",
        recording: "Alt + R",
        openApp: "Ctrl + Shift + C",
      },
      devices: {
        game: "Game audio",
        desktop: "Default output",
        mic: "Default microphone",
      },
    },
    clips,
    sources: [
      {
        id: "game",
        kind: "game",
        label: "Game audio",
        detail: "Hollow Ridge",
        gainDb: -12,
        muted: false,
        solo: false,
      },
      {
        id: "desktop",
        kind: "desktop",
        label: "Desktop audio",
        detail: "Default output",
        gainDb: -18,
        muted: false,
        solo: false,
      },
      {
        id: "mic",
        kind: "mic",
        label: "Microphone",
        detail: "Default microphone",
        gainDb: -6,
        muted: false,
        solo: false,
      },
    ],
    editorClipId: [...clips].sort(byNewest)[0]?.id ?? null,
    storage: { usedGb: 357.2, totalGb: 500 },
    lastError: null,
  };
}

export function reducer(state: AppState, action: Action): AppState {
  switch (action.type) {
    case "capture/set":
      return { ...state, capture: { ...state.capture, ...action.patch } };
    case "settings/set":
      return { ...state, settings: { ...state.settings, ...action.patch } };
    case "hotkey/set": {
      const clash = (
        Object.keys(state.settings.hotkeys) as HotkeyAction[]
      ).find(
        (k) =>
          k !== action.action && state.settings.hotkeys[k] === action.combo,
      );
      if (clash) {
        return {
          ...state,
          lastError: `${action.combo} is already used by another shortcut.`,
        };
      }
      return {
        ...state,
        lastError: null,
        settings: {
          ...state.settings,
          hotkeys: { ...state.settings.hotkeys, [action.action]: action.combo },
        },
      };
    }
    case "clip/toggleFavorite":
      return {
        ...state,
        clips: state.clips.map((c) =>
          c.id === action.id ? { ...c, favorite: !c.favorite } : c,
        ),
      };
    case "clips/delete": {
      const clips = state.clips.filter((c) => !action.ids.includes(c.id));
      const open = state.editorClipId;
      const editorClipId =
        open && clips.some((c) => c.id === open)
          ? open
          : ([...clips].sort(byNewest)[0]?.id ?? null);
      return { ...state, clips, editorClipId };
    }
    case "clips/restore": {
      const have = new Set(state.clips.map((c) => c.id));
      const clips = [
        ...state.clips,
        ...action.clips.filter((c) => !have.has(c.id)),
      ].sort(byNewest);
      const editorClipId = state.editorClipId ?? clips[0]?.id ?? null;
      return { ...state, clips, editorClipId };
    }
    case "clip/rename":
      return {
        ...state,
        clips: state.clips.map((c) =>
          c.id === action.id ? { ...c, title: action.title } : c,
        ),
      };
    case "editor/open":
      return state.clips.some((c) => c.id === action.id)
        ? { ...state, editorClipId: action.id }
        : state;
    case "source/set":
      return {
        ...state,
        sources: state.sources.map((s) =>
          s.id === action.id ? { ...s, ...action.patch } : s,
        ),
      };
    case "source/add": {
      let n = state.sources.filter((s) => s.kind === "extra").length + 1;
      while (state.sources.some((s) => s.id === `extra-${n}`)) n++;
      return {
        ...state,
        sources: [
          ...state.sources,
          {
            id: `extra-${n}`,
            kind: "extra",
            label: `Extra source ${n}`,
            detail: "Application audio",
            gainDb: -12,
            muted: false,
            solo: false,
          },
        ],
      };
    }
    case "error/clear":
      return { ...state, lastError: null };
  }
}

let state = createInitialState();
const listeners = new Set<() => void>();
const notify = () => listeners.forEach((l) => l());

export const appStore = {
  getState: () => state,
  dispatch(a: Action) {
    const next = reducer(state, a);
    if (next === state) return;
    state = next;
    notify();
  },
  subscribe(l: () => void) {
    listeners.add(l);
    return () => {
      listeners.delete(l);
    };
  },
  reset() {
    state = createInitialState();
    notify();
  },
};

export const dispatch = appStore.dispatch;

// SSR snapshot must be referentially stable across calls.
let serverState: AppState | undefined;
const getServerState = () => (serverState ??= createInitialState());

export function useApp<T>(selector: (s: AppState) => T): T {
  return useSyncExternalStore(
    appStore.subscribe,
    () => selector(appStore.getState()),
    () => selector(getServerState()),
  );
}
