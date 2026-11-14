import {
  memo,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent,
  type PointerEvent,
} from "react";
import { statusLabel } from "@/data/features";
import { useReducedMotion } from "./shared/useReducedMotion";
import { Waveform } from "./editor/Waveform";
import {
  BARS,
  DURATION,
  MIN_TRIM,
  TRACKS,
  arrowDelta,
  clamp,
  formatSeconds,
  formatTime,
  makeWave,
  wavePath,
  type TrackDef,
  type TrackId,
} from "./editor/model";

/**
 * Compact marketing demo of the clip editor. Not the real editor: sample
 * data only, nothing is decoded, exported or saved. Layout adapts to the
 * width of its own container (container queries: compact below 30rem), not
 * the viewport.
 */

const EXPORT_NAME = "Demo clip (trimmed).mp4";
const EXPORT_MS = 1800;
const TICK_MS = 80;

type ExportState = "idle" | "exporting" | "done";

// Grid rows: 1 ruler, 2 video, 3..6 audio tracks (TRACKS order).
const ROW_VIDEO = 2;
const ROW_FIRST_AUDIO = 3;
const ROW_SPAN = ROW_FIRST_AUDIO + TRACKS.length - 1;

// Ruler label sets by step. CSS (container width) shows exactly one, so
// labels thin out instead of clipping.
const LABEL_SETS = [
  { step: 15, cls: "@[21rem]:hidden" },
  { step: 10, cls: "hidden @[21rem]:block @[34rem]:hidden" },
  { step: 5, cls: "hidden @[34rem]:block" },
] as const;

const ROW_H = "h-[4.5rem] @[30rem]:h-12";
const RULER_H = "h-11 @[30rem]:h-7";
const INSET = "right-[22px] left-[22px] @[30rem]:right-2.5 @[30rem]:left-2.5";
const BELOW_RULER = "top-11 @[30rem]:top-7";
const VIDEO_BLOCKS = [18, 34, 26, 42, 22, 38, 30, 46];

const pct = (t: number) => `${(t / DURATION) * 100}%`;

function PlannedTag() {
  return (
    <span className="inline-flex shrink-0 items-center rounded-full border border-border-hover px-1.5 text-caption leading-4 font-semibold whitespace-nowrap text-text-secondary">
      {statusLabel.planned}
    </span>
  );
}

function SpeakerIcon({ muted }: { muted: boolean }) {
  return (
    <svg
      viewBox="0 0 24 24"
      className="size-4"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M11 5 6 9H3v6h3l5 4z" />
      {muted ? (
        <path d="m16 9 5 6m0-6-5 6" />
      ) : (
        <path d="M15.5 8.5a5 5 0 0 1 0 7m2.5-9.5a9 9 0 0 1 0 12" />
      )}
    </svg>
  );
}

interface RowProps {
  track: TrackDef;
  row: number;
  selected: boolean;
  muted: boolean;
  path: string;
  onSelect: (id: TrackId) => void;
  onMute: (id: TrackId) => void;
}

/** Header cell + lane cell of one audio track (re-renders only on its own state). */
const TrackRow = memo(function TrackRow({
  track,
  row,
  selected,
  muted,
  path,
  onSelect,
  onMute,
}: RowProps) {
  // Planned tracks are hidden in compact mode.
  const display = track.planned ? "hidden @[30rem]:flex" : "flex";
  return (
    <>
      <div
        style={{ gridRow: row, gridColumn: 1 }}
        className={[
          "min-w-0 flex-col items-start justify-between gap-0.5 border-b border-border px-1.5 py-1 @[30rem]:flex-row @[30rem]:items-center @[30rem]:gap-1 @[30rem]:py-0",
          ROW_H,
          display,
          selected ? "bg-accent-subtle" : "",
        ].join(" ")}
      >
        <button
          type="button"
          aria-pressed={selected}
          onClick={() => onSelect(track.id)}
          className="flex min-h-6 min-w-0 flex-1 cursor-pointer flex-col items-start justify-center gap-0.5 rounded-sm text-left text-caption text-text hover:text-accent-hover @[30rem]:min-h-9"
        >
          <span className="max-w-full truncate font-medium">{track.name}</span>
          {track.planned && <PlannedTag />}
        </button>
        <button
          type="button"
          aria-pressed={muted}
          aria-label={`Mute ${track.name}`}
          onClick={() => onMute(track.id)}
          className="inline-flex size-11 shrink-0 cursor-pointer items-center justify-center self-end rounded-md border border-border-hover bg-surface-raised text-text-secondary hover:bg-surface-hover aria-pressed:border-accent aria-pressed:bg-accent-subtle aria-pressed:text-accent-hover @[30rem]:size-8 @[30rem]:self-auto"
        >
          <SpeakerIcon muted={muted} />
        </button>
      </div>
      <div
        style={{ gridRow: row, gridColumn: 2 }}
        className={[
          "relative border-b border-border",
          ROW_H,
          track.planned ? "hidden @[30rem]:block" : "",
          selected ? "bg-accent-subtle" : "",
        ].join(" ")}
      >
        <div className={`absolute inset-y-1 ${INSET}`}>
          <Waveform
            path={path}
            bars={BARS}
            muted={muted}
            planned={track.planned}
          />
        </div>
      </div>
    </>
  );
});

const HANDLE =
  "absolute z-20 bottom-0 flex w-11 -translate-x-1/2 cursor-ew-resize touch-none items-stretch justify-center @[30rem]:w-6";
const HANDLE_BAR = "bg-accent pointer-events-none block w-1.5 rounded-sm";

export default function EditorDemo() {
  const reduced = useReducedMotion();
  const [playhead, setPlayhead] = useState(74);
  const [trim, setTrim] = useState({ start: 40, end: 215 });
  const [selected, setSelected] = useState<TrackId>("desktop");
  const [muted, setMuted] = useState<Record<TrackId, boolean>>({
    game: false,
    discord: false,
    desktop: false,
    mic: false,
  });
  const [playing, setPlaying] = useState(false);
  const [exportState, setExportState] = useState<ExportState>("idle");

  const rootRef = useRef<HTMLDivElement>(null);
  const trackRef = useRef<HTMLDivElement>(null);
  const dragRef = useRef(false);
  const playheadRef = useRef(playhead);
  const trimRef = useRef(trim);
  playheadRef.current = playhead;
  trimRef.current = trim;

  const paths = useMemo(
    () => TRACKS.map((t) => wavePath(makeWave(t.seed, t.kind))),
    [],
  );

  // ---- x -> time -------------------------------------------------------
  const timeFromX = useCallback((clientX: number) => {
    const el = trackRef.current;
    if (!el) return 0;
    const rect = el.getBoundingClientRect();
    if (rect.width <= 0) return 0;
    return clamp(
      Math.round(((clientX - rect.left) / rect.width) * DURATION),
      0,
      DURATION,
    );
  }, []);

  const seek = useCallback((t: number) => {
    setPlaying(false);
    setPlayhead(clamp(t, 0, DURATION));
  }, []);

  const changeTrim = useCallback((next: { start: number; end: number }) => {
    setTrim(next);
    setExportState("idle");
  }, []);
  const setStart = (v: number) =>
    changeTrim({
      start: clamp(v, 0, trimRef.current.end - MIN_TRIM),
      end: trimRef.current.end,
    });
  const setEnd = (v: number) =>
    changeTrim({
      start: trimRef.current.start,
      end: clamp(v, trimRef.current.start + MIN_TRIM, DURATION),
    });

  // ---- playhead drag / click-to-seek (ruler + lanes) ---------------------
  const onSurfaceDown = (e: PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    dragRef.current = true;
    e.currentTarget.setPointerCapture(e.pointerId);
    seek(timeFromX(e.clientX));
  };
  const onSurfaceMove = (e: PointerEvent<HTMLDivElement>) => {
    if (dragRef.current) seek(timeFromX(e.clientX));
  };
  const endSurfaceDrag = () => {
    dragRef.current = false;
  };

  const onPlayheadKey = (e: KeyboardEvent<HTMLDivElement>) => {
    const d = arrowDelta(e);
    if (d !== null) seek(playhead + d);
    else if (e.key === "Home") seek(0);
    else if (e.key === "End") seek(DURATION);
    else return;
    e.preventDefault();
  };

  // ---- playback: one bounded ticker, playhead -> trim end --------------------
  const togglePlayback = () => {
    if (reduced) {
      seek(trim.end);
      return;
    }
    if (playing) {
      setPlaying(false);
      return;
    }
    if (playhead < trim.start || playhead >= trim.end - 1) {
      playheadRef.current = trim.start;
      setPlayhead(trim.start);
    }
    setPlaying(true);
  };

  useEffect(() => {
    if (!playing) return;
    const from = playheadRef.current;
    const t0 = performance.now();
    const id = window.setInterval(() => {
      const next = Math.floor(from + (performance.now() - t0) / 100);
      const end = trimRef.current.end;
      if (next >= end) {
        setPlayhead(end);
        setPlaying(false);
      } else {
        setPlayhead(next);
      }
    }, TICK_MS);
    return () => window.clearInterval(id);
  }, [playing]);

  // Pause when scrolled out of view or the tab is hidden.
  useEffect(() => {
    if (!playing) return;
    const root = rootRef.current;
    const stop = () => setPlaying(false);
    const onVis = () => {
      if (document.hidden) stop();
    };
    const io =
      root && "IntersectionObserver" in window
        ? new IntersectionObserver(([entry]) => {
            if (entry && !entry.isIntersecting) stop();
          })
        : null;
    if (root) io?.observe(root);
    document.addEventListener("visibilitychange", onVis);
    return () => {
      io?.disconnect();
      document.removeEventListener("visibilitychange", onVis);
    };
  }, [playing]);

  // ---- export (simulated, one timeout) -------------------------------------
  const exportTimer = useRef<number | null>(null);
  const clearExportTimer = () => {
    if (exportTimer.current !== null) {
      window.clearTimeout(exportTimer.current);
      exportTimer.current = null;
    }
  };
  useEffect(() => clearExportTimer, []);
  // A trim edit invalidates a running or finished export.
  useEffect(() => {
    if (exportState === "idle") clearExportTimer();
  }, [exportState]);
  const runExport = () => {
    if (exportState === "exporting") return;
    setExportState("exporting");
    exportTimer.current = window.setTimeout(() => {
      exportTimer.current = null;
      setExportState("done");
    }, EXPORT_MS);
  };

  const onSelect = useCallback((id: TrackId) => setSelected(id), []);
  const onMute = useCallback(
    (id: TrackId) => setMuted((m) => ({ ...m, [id]: !m[id] })),
    [],
  );

  const outside = playhead < trim.start || playhead > trim.end;
  const trimLen = trim.end - trim.start;
  const sceneStyle: CSSProperties = {
    background: [
      `radial-gradient(circle at ${20 + (playhead / DURATION) * 60}% 32%, color-mix(in srgb, var(--color-accent) 55%, transparent) 0, transparent 38%)`,
      "linear-gradient(to bottom, color-mix(in srgb, var(--color-accent) 22%, var(--color-surface-raised)) 0, var(--color-surface) 62%, var(--color-background-elevated) 62%, var(--color-background) 100%)",
    ].join(","),
  };

  return (
    <div
      ref={rootRef}
      className="@container flex flex-col gap-4 p-3 text-text @[30rem]:p-4"
    >
      {/* Preview: abstract placeholder, not footage */}
      <div
        className="relative h-36 overflow-hidden rounded-lg border border-border @[30rem]:h-44"
        style={sceneStyle}
      >
        <div
          className={`absolute inset-0 transition-colors ${outside ? "bg-background/55" : ""}`}
          aria-hidden="true"
        />
        <span className="absolute top-2 left-2 rounded-sm bg-background/70 px-2 py-0.5 text-caption font-semibold text-text-secondary">
          Sample clip
        </span>
        {outside && (
          <span className="absolute top-2 right-2 rounded-sm bg-background/70 px-2 py-0.5 text-caption text-text-secondary">
            Outside trim
          </span>
        )}
        <span className="absolute bottom-2 left-2 rounded-sm bg-background/70 px-2 py-0.5 font-mono text-small tabular-nums">
          {formatTime(playhead)} / {formatTime(DURATION)}
        </span>
      </div>

      {/* Transport + trim readout */}
      <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
        <button
          type="button"
          className="demo-toggle"
          onClick={togglePlayback}
          aria-label={
            reduced
              ? "Jump to trim end"
              : playing
                ? "Pause preview"
                : "Play preview"
          }
        >
          {reduced ? "Jump to end" : playing ? "Pause" : "Play"}
        </button>
        <p className="font-mono text-small text-text-secondary tabular-nums">
          Trim {formatTime(trim.start)} – {formatTime(trim.end)} (
          {formatSeconds(trimLen)})
        </p>
      </div>

      {/* Timeline */}
      <div
        role="group"
        aria-label="Sample timeline"
        className="grid grid-cols-[6.5rem_minmax(0,1fr)] overflow-hidden rounded-lg border border-border bg-background-elevated @[30rem]:grid-cols-[9.5rem_minmax(0,1fr)]"
      >
        {/* Ruler row cells */}
        <div
          style={{ gridRow: 1, gridColumn: 1 }}
          className={`border-b border-border ${RULER_H}`}
        />
        <div
          style={{ gridRow: 1, gridColumn: 2 }}
          className={`border-b border-border ${RULER_H}`}
        />

        {/* Video row */}
        <div
          style={{ gridRow: ROW_VIDEO, gridColumn: 1 }}
          className={`flex items-center border-b border-border px-1.5 text-caption font-medium text-text ${ROW_H}`}
        >
          Video
        </div>
        <div
          style={{ gridRow: ROW_VIDEO, gridColumn: 2 }}
          className={`relative border-b border-border ${ROW_H}`}
        >
          <div
            className={`absolute inset-y-1.5 flex gap-px overflow-hidden rounded-sm ${INSET}`}
          >
            {VIDEO_BLOCKS.map((w, i) => (
              <div
                key={i}
                className="h-full"
                style={{
                  flex: `${w} 1 0`,
                  background: `linear-gradient(135deg, color-mix(in srgb, var(--color-accent) ${18 + ((i * 13) % 30)}%, var(--color-surface-raised)), var(--color-surface-raised))`,
                }}
              />
            ))}
          </div>
        </div>

        {TRACKS.map((track, i) => (
          <TrackRow
            key={track.id}
            track={track}
            row={ROW_FIRST_AUDIO + i}
            selected={selected === track.id}
            muted={muted[track.id]}
            path={paths[i] ?? ""}
            onSelect={onSelect}
            onMute={onMute}
          />
        ))}

        {/* Interactive overlay over the lane column */}
        <div
          style={{ gridRow: `1 / span ${ROW_SPAN}`, gridColumn: 2 }}
          className="relative z-10 touch-pan-y select-none"
          onPointerDown={onSurfaceDown}
          onPointerMove={onSurfaceMove}
          onPointerUp={endSurfaceDrag}
          onPointerCancel={endSurfaceDrag}
          onLostPointerCapture={endSurfaceDrag}
        >
          <div ref={trackRef} className={`absolute inset-y-0 ${INSET}`}>
            {/* Ruler: trim tint + labels */}
            <div
              className={`pointer-events-none absolute inset-x-0 top-0 ${RULER_H}`}
            >
              <div
                className="absolute inset-y-0 bg-accent-subtle"
                style={{ left: pct(trim.start), width: pct(trimLen) }}
              />
              {LABEL_SETS.map((set) => (
                <div
                  key={set.step}
                  className={`absolute inset-0 ${set.cls}`}
                  aria-hidden="true"
                >
                  {Array.from(
                    { length: DURATION / 10 / set.step + 1 },
                    (_, k) => {
                      const sec = k * set.step;
                      const align =
                        sec === 0
                          ? "none"
                          : sec * 10 === DURATION
                            ? "translateX(-100%)"
                            : "translateX(-50%)";
                      return (
                        <span
                          key={sec}
                          className="absolute top-1 font-mono text-caption text-text-muted tabular-nums"
                          style={{ left: pct(sec * 10), transform: align }}
                        >
                          {formatTime(sec * 10).slice(0, -2)}
                        </span>
                      );
                    },
                  )}
                </div>
              ))}
            </div>

            {/* Dim outside the trim range (below the ruler) */}
            <div
              className={`pointer-events-none absolute bottom-0 left-0 bg-background/70 ${BELOW_RULER}`}
              style={{ width: pct(trim.start) }}
              aria-hidden="true"
            />
            <div
              className={`pointer-events-none absolute right-0 bottom-0 bg-background/70 ${BELOW_RULER}`}
              style={{ width: pct(DURATION - trim.end) }}
              aria-hidden="true"
            />

            {/* Trim handles */}
            <div
              role="slider"
              tabIndex={0}
              aria-label="Trim start"
              aria-orientation="horizontal"
              aria-valuemin={0}
              aria-valuemax={trim.end - MIN_TRIM}
              aria-valuenow={trim.start}
              aria-valuetext={formatTime(trim.start)}
              className={`${HANDLE} ${BELOW_RULER}`}
              style={{ left: pct(trim.start) }}
              onPointerDown={(e) => {
                e.stopPropagation();
                e.currentTarget.setPointerCapture(e.pointerId);
              }}
              onPointerMove={(e) => {
                if (e.currentTarget.hasPointerCapture(e.pointerId))
                  setStart(timeFromX(e.clientX));
              }}
              onKeyDown={(e) => {
                const d = arrowDelta(e);
                if (d !== null) setStart(trim.start + d);
                else if (e.key === "Home") setStart(0);
                else if (e.key === "End") setStart(trim.end - MIN_TRIM);
                else return;
                e.preventDefault();
              }}
            >
              <span className={HANDLE_BAR} />
            </div>
            <div
              role="slider"
              tabIndex={0}
              aria-label="Trim end"
              aria-orientation="horizontal"
              aria-valuemin={trim.start + MIN_TRIM}
              aria-valuemax={DURATION}
              aria-valuenow={trim.end}
              aria-valuetext={formatTime(trim.end)}
              className={`${HANDLE} ${BELOW_RULER}`}
              style={{ left: pct(trim.end) }}
              onPointerDown={(e) => {
                e.stopPropagation();
                e.currentTarget.setPointerCapture(e.pointerId);
              }}
              onPointerMove={(e) => {
                if (e.currentTarget.hasPointerCapture(e.pointerId))
                  setEnd(timeFromX(e.clientX));
              }}
              onKeyDown={(e) => {
                const d = arrowDelta(e);
                if (d !== null) setEnd(trim.end + d);
                else if (e.key === "Home") setEnd(trim.start + MIN_TRIM);
                else if (e.key === "End") setEnd(DURATION);
                else return;
                e.preventDefault();
              }}
            >
              <span className={HANDLE_BAR} />
            </div>

            {/* Playhead: line + slider knob in the ruler */}
            <div
              className="pointer-events-none absolute inset-y-0 z-10 w-0.5 -translate-x-1/2 bg-text"
              style={{ left: pct(playhead) }}
              aria-hidden="true"
            />
            <div
              role="slider"
              tabIndex={0}
              aria-label="Playhead"
              aria-orientation="horizontal"
              aria-valuemin={0}
              aria-valuemax={DURATION}
              aria-valuenow={playhead}
              aria-valuetext={`${formatTime(playhead)} of ${formatTime(DURATION)}`}
              onKeyDown={onPlayheadKey}
              className={`absolute top-0 z-30 flex w-11 -translate-x-1/2 cursor-ew-resize touch-none items-start justify-center @[30rem]:w-6 ${RULER_H}`}
              style={{ left: pct(playhead) }}
            >
              <span className="pointer-events-none mt-0.5 block h-3 w-3 rounded-sm bg-text" />
            </div>
          </div>
        </div>
      </div>

      {/* Honesty note + compact-width note */}
      <div className="flex flex-col gap-1 text-caption text-text-secondary">
        <p className="flex flex-wrap items-center gap-x-2 gap-y-1">
          <PlannedTag />
          <span>Per-track audio editing is planned. Trimming works today.</span>
        </p>
        <p className="@[30rem]:hidden">
          +2 planned tracks (Game audio, Discord) are hidden at this width.
        </p>
      </div>

      {/* Export (simulated) */}
      <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
        <button
          type="button"
          onClick={runExport}
          disabled={exportState === "exporting"}
          className="inline-flex min-h-11 cursor-pointer items-center justify-center rounded-md bg-accent px-4 text-small font-semibold text-accent-foreground hover:bg-accent-hover disabled:cursor-progress disabled:opacity-60"
        >
          {exportState === "exporting" ? "Exporting…" : "Export trimmed clip"}
        </button>
        <p
          role="status"
          className="min-h-9 min-w-0 flex-1 basis-48 text-caption text-text-secondary"
        >
          {exportState === "exporting" && "Exporting…"}
          {exportState === "done" &&
            `Exported '${EXPORT_NAME}'. Simulated: no file was created.`}
          {exportState === "idle" && "Demo only: export does not save a file."}
        </p>
      </div>
    </div>
  );
}
