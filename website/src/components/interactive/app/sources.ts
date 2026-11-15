import type { IconName } from "./ui/Icon";
import { GAMES } from "./mockData";

/** Capture sources offered by the top bar / Capture page source pickers. */
export interface CaptureSource {
  /** Matches `CaptureState.sourceId`; game sources reuse the game id. */
  id: string;
  name: string;
  detail: string;
  kind: "game" | "monitor" | "window";
  icon: IconName;
}

export const CAPTURE_SOURCES: CaptureSource[] = [
  ...GAMES.map((g): CaptureSource => ({
    id: g.id,
    name: g.name,
    detail: "Game Capture",
    kind: "game",
    icon: "gamepad",
  })),
  {
    id: "display-1",
    name: "Display 1",
    detail: "Monitor Capture",
    kind: "monitor",
    icon: "monitor",
  },
  {
    id: "window-browser",
    name: "Browser window",
    detail: "Window Capture",
    kind: "window",
    icon: "window",
  },
];

export function captureSourceById(id: string): CaptureSource {
  return CAPTURE_SOURCES.find((s) => s.id === id) ?? CAPTURE_SOURCES[0];
}
