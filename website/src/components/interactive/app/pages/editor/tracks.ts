import type { IconName } from "../../ui/Icon";
import type { WaveKind } from "../../../editor/model";
import type { TrackId } from "./editorState";

export interface EditorTrack {
  id: TrackId;
  name: string;
  icon: IconName;
  /** Accent token for the lane; the window accent unless set. */
  colorVar?: string;
  wave?: { seed: number; kind: WaveKind };
}

/** Video plus three audio tracks, as in the mockup. */
export const EDITOR_TRACKS: readonly EditorTrack[] = [
  { id: "video", name: "Video", icon: "film" },
  {
    id: "game",
    name: "Game Audio",
    icon: "gamepad",
    wave: { seed: 11, kind: "game" },
  },
  {
    id: "desktop",
    name: "Desktop Audio",
    icon: "monitor",
    colorVar: "--app-accent-blue",
    wave: { seed: 37, kind: "steady" },
  },
  {
    id: "mic",
    name: "Microphone",
    icon: "mic",
    colorVar: "--app-accent-green",
    wave: { seed: 53, kind: "bursty" },
  },
];

export interface TrackFlags {
  locked: boolean;
  hidden: boolean;
  muted: boolean;
}

export const NO_FLAGS: TrackFlags = {
  locked: false,
  hidden: false,
  muted: false,
};
