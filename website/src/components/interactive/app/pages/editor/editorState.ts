/**
 * Pure edit model of the tour's Editor page. Times are integer tenths of a
 * second (same unit as ../../../editor/model.ts). The page keeps one
 * `History` per open clip; trims, splits and deleted segments are undoable.
 */
import { MIN_TRIM, clamp } from "../../../editor/model";

export type TrackId = "video" | "game" | "desktop" | "mic";
export const TRACK_IDS: readonly TrackId[] = [
  "video",
  "game",
  "desktop",
  "mic",
];

export interface Range {
  start: number;
  end: number;
}

export interface Doc {
  duration: number;
  trims: Record<TrackId, Range>;
  /** Split points on the video track, sorted. */
  cuts: number[];
  /** Deleted video segments (between cuts / clip bounds). */
  removed: Range[];
}

export interface History {
  past: Doc[];
  present: Doc;
  future: Doc[];
  /** Doc at the start of a running drag (one undo step per drag). */
  anchor: Doc | null;
}

/** Initial audio trims as fractions of the clip, like the mockup. */
const START_FRACTIONS: Record<TrackId, [number, number]> = {
  video: [0, 1],
  game: [0.15, 0.62],
  desktop: [0, 1],
  mic: [0.14, 0.65],
};

export function initialDoc(duration: number): Doc {
  const d = Math.max(MIN_TRIM, Math.round(duration));
  const trims = {} as Record<TrackId, Range>;
  for (const id of TRACK_IDS) {
    const [a, b] = START_FRACTIONS[id];
    trims[id] = { start: Math.round(a * d), end: Math.round(b * d) };
  }
  return { duration: d, trims, cuts: [], removed: [] };
}

export function initialHistory(duration: number): History {
  return { past: [], present: initialDoc(duration), future: [], anchor: null };
}

/** Move one trim edge. Start stays <= end - MIN_TRIM and vice versa. */
export function setEdge(
  doc: Doc,
  track: TrackId,
  edge: "start" | "end",
  value: number,
): Doc {
  const cur = doc.trims[track];
  const v = Math.round(value);
  const next =
    edge === "start"
      ? { start: clamp(v, 0, cur.end - MIN_TRIM), end: cur.end }
      : { start: cur.start, end: clamp(v, cur.start + MIN_TRIM, doc.duration) };
  if (next.start === cur.start && next.end === cur.end) return doc;
  return { ...doc, trims: { ...doc.trims, [track]: next } };
}

export function resetTrim(doc: Doc, track: TrackId): Doc {
  const fresh = initialDoc(doc.duration).trims[track];
  const cur = doc.trims[track];
  if (fresh.start === cur.start && fresh.end === cur.end) return doc;
  return { ...doc, trims: { ...doc.trims, [track]: fresh } };
}

/** Video segments between the clip bounds and every cut. */
export function segments(doc: Doc): Range[] {
  const pts = [0, ...doc.cuts, doc.duration];
  const out: Range[] = [];
  for (let i = 0; i < pts.length - 1; i++) {
    out.push({ start: pts[i]!, end: pts[i + 1]! });
  }
  return out;
}

export function isRemoved(doc: Doc, seg: Range): boolean {
  return doc.removed.some((r) => r.start === seg.start && r.end === seg.end);
}

/** Split the video track at `t`. No-op at the bounds or an existing cut. */
export function split(doc: Doc, t: number): Doc {
  const at = Math.round(t);
  if (at <= 0 || at >= doc.duration || doc.cuts.includes(at)) return doc;
  // A cut inside a deleted segment would orphan it; keep it whole.
  if (doc.removed.some((r) => at > r.start && at < r.end)) return doc;
  return { ...doc, cuts: [...doc.cuts, at].sort((a, b) => a - b) };
}

/** Delete the video segment under `t` (needs at least one cut). */
export function deleteAt(doc: Doc, t: number): Doc {
  const segs = segments(doc);
  const seg = segs.find((s) => t >= s.start && t < s.end) ?? segs.at(-1);
  if (!seg || segs.length < 2 || isRemoved(doc, seg)) return doc;
  const kept = segs.filter((s) => s !== seg && !isRemoved(doc, s));
  if (kept.length === 0) return doc;
  return { ...doc, removed: [...doc.removed, seg] };
}

export type HistoryAction =
  | { type: "edit"; fn: (d: Doc) => Doc }
  | { type: "dragStart" }
  | { type: "dragEnd" }
  | { type: "undo" }
  | { type: "redo" }
  | { type: "reset"; duration: number };

export function historyReducer(h: History, a: HistoryAction): History {
  switch (a.type) {
    case "edit": {
      const next = a.fn(h.present);
      if (next === h.present) return h;
      // While dragging, only the drag end records an undo step.
      if (h.anchor) return { ...h, present: next };
      return {
        past: [...h.past, h.present],
        present: next,
        future: [],
        anchor: null,
      };
    }
    case "dragStart":
      return h.anchor ? h : { ...h, anchor: h.present };
    case "dragEnd": {
      if (!h.anchor) return h;
      if (h.anchor === h.present) return { ...h, anchor: null };
      return {
        past: [...h.past, h.anchor],
        present: h.present,
        future: [],
        anchor: null,
      };
    }
    case "undo": {
      if (h.anchor || h.past.length === 0) return h;
      return {
        past: h.past.slice(0, -1),
        present: h.past.at(-1)!,
        future: [h.present, ...h.future],
        anchor: null,
      };
    }
    case "redo": {
      if (h.anchor || h.future.length === 0) return h;
      return {
        past: [...h.past, h.present],
        present: h.future[0]!,
        future: h.future.slice(1),
        anchor: null,
      };
    }
    case "reset":
      return initialHistory(a.duration);
  }
}

/** 124 -> "00:12.400" (the mockup's mm:ss.mmm). */
export function formatClock(tenths: number): string {
  const t = Math.max(0, Math.round(tenths));
  const m = Math.floor(t / 600);
  const s = Math.floor(t / 10) % 60;
  return `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}.${t % 10}00`;
}

/** "00:12.400", "1:05", "12.5" -> tenths; null when not a time. */
export function parseClock(text: string): number | null {
  const m = /^\s*(?:(\d{1,3}):)?(\d{1,3})(?:\.(\d{1,3}))?\s*$/.exec(text);
  if (!m) return null;
  const minutes = m[1] ? Number(m[1]) : 0;
  const seconds = Number(m[2]);
  if (m[1] && seconds >= 60) return null;
  const frac = m[3] ? Number(`0.${m[3]}`) : 0;
  return Math.round((minutes * 60 + seconds + frac) * 10);
}
