/** Pure model for the editor demo. All times are integer tenths of a second. */

export const DURATION = 300;
export const MIN_TRIM = 10;
export const STEP = 1;
export const BIG_STEP = 10;

export type TrackId = "game" | "discord" | "desktop" | "mic";

export interface TrackDef {
  id: TrackId;
  name: string;
  /** Not available in the app yet; shown tagged "Planned". */
  planned: boolean;
  seed: number;
  kind: "steady" | "bursty" | "game";
}

export const TRACKS: readonly TrackDef[] = [
  { id: "game", name: "Game audio", planned: true, seed: 11, kind: "game" },
  { id: "discord", name: "Discord", planned: true, seed: 23, kind: "bursty" },
  {
    id: "desktop",
    name: "Desktop audio",
    planned: false,
    seed: 37,
    kind: "steady",
  },
  {
    id: "mic",
    name: "Microphone",
    planned: false,
    seed: 53,
    kind: "bursty",
  },
];

export function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

/** 74 -> "0:07.4" */
export function formatTime(tenths: number): string {
  const t = Math.max(0, Math.round(tenths));
  const minutes = Math.floor(t / 600);
  const seconds = Math.floor(t / 10) % 60;
  return `${minutes}:${String(seconds).padStart(2, "0")}.${t % 10}`;
}

/** 175 -> "17.5 s" */
export function formatSeconds(tenths: number): string {
  return `${(tenths / 10).toFixed(1)} s`;
}

function mulberry32(seed: number): () => number {
  let a = seed;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

export const BARS = 72;

/** Deterministic pseudo waveform, values in 0.06..1. */
export function makeWave(
  seed: number,
  kind: TrackDef["kind"],
  n = BARS,
): number[] {
  const rand = mulberry32(seed);
  const out: number[] = [];
  if (kind === "bursty") {
    let on = rand() > 0.5;
    let left = 3 + Math.floor(rand() * 6);
    for (let i = 0; i < n; i++) {
      if (left-- <= 0) {
        on = !on;
        left = on ? 4 + Math.floor(rand() * 8) : 2 + Math.floor(rand() * 6);
      }
      out.push(on ? 0.3 + rand() * 0.7 : 0.06 + rand() * 0.1);
    }
  } else if (kind === "game") {
    for (let i = 0; i < n; i++) {
      const swell = 0.5 + 0.5 * Math.sin(i / 6 + seed);
      const hit = rand() > 0.82 ? 0.35 : 0;
      out.push(clamp(0.18 + swell * 0.45 + hit + rand() * 0.2, 0.06, 1));
    }
  } else {
    for (let i = 0; i < n; i++) {
      out.push(clamp(0.28 + 0.14 * Math.sin(i / 5) + rand() * 0.22, 0.06, 1));
    }
  }
  return out;
}

/** One SVG path (viewBox 0 0 n 100) drawing every bar as a centered rect. */
export function wavePath(values: readonly number[]): string {
  return values
    .map((v, i) => {
      const h = Math.max(4, v * 92);
      return `M${i + 0.19} ${(50 - h / 2).toFixed(1)}h0.62v${h.toFixed(1)}h-0.62z`;
    })
    .join("");
}

/** Keyboard delta for arrow keys, or null if the key is not an arrow. */
export function arrowDelta(e: {
  key: string;
  shiftKey: boolean;
}): number | null {
  const step = e.shiftKey ? BIG_STEP : STEP;
  if (e.key === "ArrowRight" || e.key === "ArrowUp") return step;
  if (e.key === "ArrowLeft" || e.key === "ArrowDown") return -step;
  return null;
}
