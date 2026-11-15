import {
  memo,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type PointerEvent,
} from "react";
import { ClipThumb, type ThumbScene } from "../../thumbs";
import { Icon, type IconName } from "../../ui/Icon";
import { IconButton } from "../../ui/IconButton";
import { PlannedBadge } from "../../ui/PlannedBadge";
import { Slider } from "../../ui/Slider";
import { Waveform } from "../../../editor/Waveform";
import {
  MIN_TRIM,
  arrowDelta,
  clamp,
  makeWave,
  wavePath,
} from "../../../editor/model";
import {
  formatClock,
  isRemoved,
  segments,
  setEdge,
  type Doc,
  type TrackId,
} from "./editorState";
import { EDITOR_TRACKS, type EditorTrack, type TrackFlags } from "./tracks";

export type Tool = "select" | "razor";
export const ZOOM_MIN = 1;
export const ZOOM_MAX = 4;
const WAVE_BARS = 140;
const FILM_FRAMES = 10;
const LABEL_STEPS = [1, 2, 5, 10, 15, 30, 60, 120, 300];

const ROW = "h-9 @min-[60rem]/app-window:h-7";
const RULER = "h-7 @min-[60rem]/app-window:h-6";

interface Props {
  doc: Doc;
  playhead: number;
  scene: ThumbScene;
  seed: number;
  flags: Record<TrackId, TrackFlags>;
  onFlag(id: TrackId, key: keyof TrackFlags): void;
  zoom: number;
  onZoom(z: number): void;
  tool: Tool;
  onTool(t: Tool): void;
  snap: boolean;
  onSnap(): void;
  link: boolean;
  onLink(): void;
  canUndo: boolean;
  canRedo: boolean;
  onUndo(): void;
  onRedo(): void;
  onEdit(fn: (d: Doc) => Doc): void;
  onDragStart(): void;
  onDragEnd(): void;
  onSeek(t: number): void;
  onSplitAt(t: number): void;
  onResetTrim(id: TrackId): void;
  notify(msg: string): void;
}

const pct = (t: number, d: number) => `${(t / d) * 100}%`;
const mmss = (tenths: number) => formatClock(tenths).slice(0, 5);

/** Ghost toggle used in the track headers (lock, eye, speaker). */
function TrackToggle({
  label,
  pressed,
  icon,
  onClick,
}: {
  label: string;
  pressed: boolean;
  icon: IconName;
  onClick(): void;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      aria-pressed={pressed}
      onClick={onClick}
      className={`grid size-7 shrink-0 cursor-pointer place-items-center rounded-md transition-colors hover:bg-surface-hover pointer-coarse:size-9 ${pressed ? "bg-accent-subtle text-text" : "text-text-secondary hover:text-text"}`}
    >
      <Icon name={icon} className="size-4" />
    </button>
  );
}

function TrackHeader({
  track,
  flags,
  onFlag,
  onResetTrim,
}: {
  track: EditorTrack;
  flags: TrackFlags;
  onFlag: Props["onFlag"];
  onResetTrim: Props["onResetTrim"];
}) {
  const [menu, setMenu] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!menu) return;
    const close = (e: Event) => {
      if (!menuRef.current?.contains(e.target as Node)) setMenu(false);
    };
    document.addEventListener("pointerdown", close);
    return () => document.removeEventListener("pointerdown", close);
  }, [menu]);
  const color = track.colorVar
    ? ({ "--color-accent": `var(${track.colorVar})` } as CSSProperties)
    : undefined;
  return (
    <div
      className={`flex ${ROW} min-w-0 items-center gap-0.5 rounded-md bg-surface-raised px-1`}
    >
      <TrackToggle
        label={`Lock ${track.name}`}
        pressed={flags.locked}
        icon={flags.locked ? "lock" : "unlock"}
        onClick={() => onFlag(track.id, "locked")}
      />
      <TrackToggle
        label={`Hide ${track.name}`}
        pressed={flags.hidden}
        icon={flags.hidden ? "eyeOff" : "eye"}
        onClick={() => onFlag(track.id, "hidden")}
      />
      <TrackToggle
        label={`Mute ${track.name}`}
        pressed={flags.muted}
        icon={flags.muted ? "volumeMute" : "volume"}
        onClick={() => onFlag(track.id, "muted")}
      />
      <span style={color} className="ml-1 shrink-0 text-accent">
        <Icon
          name={track.icon}
          className={`size-4 ${track.id === "video" ? "text-text-secondary" : ""}`}
        />
      </span>
      <span className="app-text-sm sr-only min-w-0 flex-1 truncate pl-1.5 text-text @min-[48rem]/app-window:not-sr-only @min-[48rem]/app-window:whitespace-nowrap">
        {track.name}
      </span>
      <div ref={menuRef} className="relative ml-auto">
        <button
          type="button"
          aria-label={`${track.name} options`}
          aria-haspopup="menu"
          aria-expanded={menu}
          onClick={() => setMenu((m) => !m)}
          onKeyDown={(e) => {
            if (e.key === "Escape" && menu) {
              e.stopPropagation();
              setMenu(false);
            }
          }}
          className="grid size-7 cursor-pointer place-items-center rounded-md text-text-secondary hover:bg-surface-hover hover:text-text"
        >
          <Icon name="more" className="size-4" />
        </button>
        {menu && (
          <div
            role="menu"
            aria-label={`${track.name} options`}
            className="absolute top-full left-0 z-40 mt-1 min-w-36 rounded-md border border-border-hover bg-surface-raised p-1 shadow-md"
          >
            <button
              type="button"
              role="menuitem"
              autoFocus
              onClick={() => {
                onResetTrim(track.id);
                setMenu(false);
              }}
              onKeyDown={(e) => {
                if (e.key === "Escape") {
                  e.stopPropagation();
                  setMenu(false);
                }
              }}
              className="app-text-sm w-full cursor-pointer rounded-sm px-2 py-1.5 text-left text-text hover:bg-surface-hover"
            >
              Reset trim
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

/** Filmstrip of the clip's scene; memoized so playhead ticks skip it. */
const Filmstrip = memo(function Filmstrip({
  scene,
  seed,
}: {
  scene: ThumbScene;
  seed: number;
}) {
  return (
    <div
      className="absolute inset-0 flex overflow-hidden rounded-md"
      aria-hidden="true"
    >
      {Array.from({ length: FILM_FRAMES }, (_, i) => (
        <div key={i} className="h-full min-w-0 flex-1 border-r border-surface">
          <ClipThumb
            scene={scene}
            seed={seed + (i % 3)}
            className="pointer-events-none h-full w-full"
          />
        </div>
      ))}
    </div>
  );
});

interface HandleProps {
  track: EditorTrack;
  edge: "start" | "end";
  value: number;
  min: number;
  max: number;
  locked: boolean;
  timeFromX(x: number): number;
  onEdit: Props["onEdit"];
  onDragStart(): void;
  onDragEnd(): void;
}

function TrimHandle({
  track,
  edge,
  value,
  min,
  max,
  locked,
  timeFromX,
  onEdit,
  onDragStart,
  onDragEnd,
}: HandleProps) {
  const grab = useRef(0);
  const move = (v: number) => onEdit((d) => setEdge(d, track.id, edge, v));
  const end = () => onDragEnd();
  return (
    <div
      role="slider"
      tabIndex={locked ? -1 : 0}
      aria-label={`${track.name} ${edge}`}
      aria-orientation="horizontal"
      aria-disabled={locked}
      aria-valuemin={min}
      aria-valuemax={max}
      aria-valuenow={value}
      aria-valuetext={formatClock(value)}
      onPointerDown={(e) => {
        e.stopPropagation();
        if (locked || e.button !== 0) return;
        e.currentTarget.setPointerCapture(e.pointerId);
        grab.current = timeFromX(e.clientX) - value;
        onDragStart();
      }}
      onPointerMove={(e) => {
        if (e.currentTarget.hasPointerCapture(e.pointerId))
          move(timeFromX(e.clientX) - grab.current);
      }}
      onPointerUp={end}
      onPointerCancel={end}
      onLostPointerCapture={end}
      onKeyDown={(e) => {
        if (locked) return;
        const d = arrowDelta(e);
        if (d !== null) move(value + d);
        else if (e.key === "Home") move(min);
        else if (e.key === "End") move(max);
        else return;
        e.preventDefault();
      }}
      className={`absolute inset-y-0 z-20 flex w-3 touch-none items-center justify-center bg-accent text-accent-foreground pointer-coarse:w-5 ${edge === "start" ? "left-0 rounded-l-md" : "right-0 rounded-r-md"} ${locked ? "cursor-not-allowed opacity-50" : "cursor-ew-resize"}`}
    >
      <Icon
        name={edge === "start" ? "chevronLeft" : "chevronRight"}
        className="pointer-events-none size-3"
      />
    </div>
  );
}

function Lane({
  track,
  doc,
  flags,
  path,
  scene,
  seed,
  timeFromX,
  onEdit,
  onDragStart,
  onDragEnd,
}: {
  track: EditorTrack;
  doc: Doc;
  flags: TrackFlags;
  path: string;
  scene: ThumbScene;
  seed: number;
  timeFromX(x: number): number;
  onEdit: Props["onEdit"];
  onDragStart(): void;
  onDragEnd(): void;
}) {
  const d = doc.duration;
  const trim = doc.trims[track.id];
  const color = track.colorVar
    ? ({ "--color-accent": `var(${track.colorVar})` } as CSSProperties)
    : undefined;
  const isVideo = track.id === "video";
  const common = {
    track,
    locked: flags.locked,
    timeFromX,
    onEdit,
    onDragStart,
    onDragEnd,
  };
  return (
    <div
      style={color}
      data-track={track.id}
      className={`relative ${ROW} rounded-md bg-surface-raised transition-opacity ${flags.hidden ? "opacity-40" : ""}`}
    >
      {isVideo ? (
        <div className="absolute inset-0.5">
          <Filmstrip scene={scene} seed={seed} />
        </div>
      ) : (
        <div className="absolute inset-x-0 inset-y-1">
          <Waveform
            path={path}
            bars={WAVE_BARS}
            muted={flags.muted}
            planned={false}
          />
        </div>
      )}
      {/* Dim outside the trim range */}
      <div
        className="pointer-events-none absolute inset-y-0 left-0 rounded-l-md bg-background/60"
        style={{ width: pct(trim.start, d) }}
        aria-hidden="true"
      />
      <div
        className="pointer-events-none absolute inset-y-0 right-0 rounded-r-md bg-background/60"
        style={{ width: pct(d - trim.end, d) }}
        aria-hidden="true"
      />
      {isVideo &&
        segments(doc)
          .filter((s) => isRemoved(doc, s))
          .map((s) => (
            <div
              key={s.start}
              className="app-text-xs pointer-events-none absolute inset-y-0 z-10 grid place-items-center overflow-hidden bg-background/85 text-text-muted"
              style={{ left: pct(s.start, d), width: pct(s.end - s.start, d) }}
            >
              Deleted
            </div>
          ))}
      {isVideo &&
        doc.cuts.map((c) => (
          <div
            key={c}
            aria-hidden="true"
            className="pointer-events-none absolute inset-y-0 z-10 w-0.5 -translate-x-1/2 bg-text"
            style={{ left: pct(c, d) }}
          />
        ))}
      {/* Trim box with handles */}
      <div
        className={`absolute inset-y-0 z-10 rounded-md border-2 border-accent ${isVideo ? "" : "bg-accent-subtle"}`}
        style={{
          left: pct(trim.start, d),
          width: pct(trim.end - trim.start, d),
        }}
      >
        <TrimHandle
          {...common}
          edge="start"
          value={trim.start}
          min={0}
          max={trim.end - MIN_TRIM}
        />
        <TrimHandle
          {...common}
          edge="end"
          value={trim.end}
          min={trim.start + MIN_TRIM}
          max={d}
        />
      </div>
    </div>
  );
}

/**
 * Timeline: toolbar (undo/redo, tools, snap, zoom), ruler with playhead,
 * one video lane and three audio lanes with trim handles. Wide content
 * scrolls inside the window.
 */
export function Timeline(p: Props) {
  const { doc, playhead, zoom } = p;
  const d = doc.duration;
  const lanesRef = useRef<HTMLDivElement>(null);
  const seeking = useRef(false);
  const paths = useMemo(
    () =>
      EDITOR_TRACKS.map((t) =>
        t.wave ? wavePath(makeWave(t.wave.seed, t.wave.kind, WAVE_BARS)) : "",
      ),
    [],
  );

  const timeFromX = (x: number) => {
    const el = lanesRef.current;
    if (!el) return 0;
    const r = el.getBoundingClientRect();
    if (r.width <= 0) return 0;
    return clamp(Math.round(((x - r.left) / r.width) * d), 0, d);
  };

  const sec = d / 10;
  const step =
    LABEL_STEPS.find((s) => sec / s <= 6 * zoom) ?? LABEL_STEPS.at(-1)!;
  const labels = Array.from(
    { length: Math.floor(sec / step) + 1 },
    (_, i) => i * step * 10,
  );
  const minor = Array.from(
    { length: Math.floor((sec / step) * 5) + 1 },
    (_, i) => (i * step * 10) / 5,
  );

  const onSurfaceDown = (e: PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    const t = timeFromX(e.clientX);
    const lane = (e.target as HTMLElement).closest("[data-track]");
    if (p.tool === "razor" && lane?.getAttribute("data-track") === "video") {
      if (p.flags.video.locked) p.notify("The video track is locked.");
      else p.onSplitAt(t);
      return;
    }
    seeking.current = true;
    e.currentTarget.setPointerCapture(e.pointerId);
    p.onSeek(t);
  };

  const zoomBy = (delta: number) =>
    p.onZoom(clamp(zoom + delta, ZOOM_MIN, ZOOM_MAX));

  return (
    <section
      aria-label="Timeline"
      className="flex flex-col gap-1 rounded-xl border border-border bg-surface p-1.5"
    >
      {/* Toolbar */}
      <div className="flex flex-wrap items-center gap-1.5">
        <IconButton
          label="Undo"
          icon="undo"
          size="sm"
          onClick={p.onUndo}
          disabled={!p.canUndo}
        />
        <IconButton
          label="Redo"
          icon="redo"
          size="sm"
          onClick={p.onRedo}
          disabled={!p.canRedo}
        />
        <span aria-hidden="true" className="mx-1 h-5 w-px bg-border" />
        <IconButton
          label="Select tool"
          icon="select"
          size="sm"
          pressed={p.tool === "select"}
          onClick={() => p.onTool("select")}
        />
        <IconButton
          label="Razor tool (click the video track to split)"
          icon="razor"
          size="sm"
          pressed={p.tool === "razor"}
          onClick={() => p.onTool("razor")}
        />
        <span
          className="inline-flex items-center gap-1.5"
          data-element="editor.linkTracks"
        >
          <IconButton
            label="Link audio to video"
            icon="link"
            size="sm"
            pressed={p.link}
            onClick={p.onLink}
          />
          <PlannedBadge id="editor.linkTracks" />
        </span>
        <IconButton
          label="Snap to playhead"
          icon="magnet"
          size="sm"
          pressed={p.snap}
          onClick={p.onSnap}
          plannedId="editor.snap"
        />
        <span
          className="inline-flex items-center gap-1.5 pl-1"
          data-element="editor.trackLock"
        >
          <Icon name="lock" className="size-3.5 text-text-muted" />
          <span className="app-text-xs text-text-muted">Track lock</span>
          <PlannedBadge id="editor.trackLock" />
        </span>
        <div
          className="ml-auto flex items-center gap-1.5"
          data-element="editor.zoom"
        >
          <IconButton
            label="Zoom out"
            icon="minus"
            size="sm"
            onClick={() => zoomBy(-0.5)}
            disabled={zoom <= ZOOM_MIN}
          />
          <Slider
            label="Timeline zoom"
            min={ZOOM_MIN}
            max={ZOOM_MAX}
            step={0.5}
            value={zoom}
            onChange={p.onZoom}
            format={(v) => `${v * 100}%`}
            className="w-16 @min-[48rem]/app-window:w-24"
          />
          <IconButton
            label="Zoom in"
            icon="plus"
            size="sm"
            onClick={() => zoomBy(0.5)}
            disabled={zoom >= ZOOM_MAX}
          />
          <IconButton
            label="Fit timeline"
            icon="monitor"
            size="sm"
            onClick={() => p.onZoom(ZOOM_MIN)}
          />
          <PlannedBadge id="editor.zoom" />
        </div>
      </div>

      <div
        className="grid min-h-0 grid-cols-[auto_minmax(0,1fr)] gap-x-2"
        data-element="editor.multitrack"
      >
        {/* Headers */}
        <div className="flex w-[8.5rem] flex-col gap-1 @min-[48rem]/app-window:w-[15rem]">
          <span
            className={`flex ${RULER} items-center gap-1.5`}
            data-element="editor.addTrack"
          >
            <button
              type="button"
              onClick={() => p.notify("Adding tracks is planned.")}
              className="app-text-xs inline-flex h-full cursor-pointer items-center gap-1 rounded-md border border-border bg-surface-raised px-2 whitespace-nowrap text-text hover:bg-surface-hover"
            >
              <Icon name="plus" className="size-3.5" />
              <span className="sr-only @min-[48rem]/app-window:not-sr-only">
                Add Track
              </span>
            </button>
            <PlannedBadge id="editor.addTrack" />
          </span>
          {EDITOR_TRACKS.map((t) => (
            <TrackHeader
              key={t.id}
              track={t}
              flags={p.flags[t.id]}
              onFlag={p.onFlag}
              onResetTrim={p.onResetTrim}
            />
          ))}
        </div>

        {/* Lanes (scroll horizontally when zoomed or narrow) */}
        <div
          data-timeline-lanes
          className="min-w-0 overflow-x-auto overflow-y-hidden pb-1"
        >
          <div
            className="relative min-w-[22rem] touch-pan-y px-1 select-none"
            style={{ width: `${zoom * 100}%` }}
            onPointerDown={onSurfaceDown}
            onPointerMove={(e) => {
              if (seeking.current) p.onSeek(timeFromX(e.clientX));
            }}
            onPointerUp={() => (seeking.current = false)}
            onPointerCancel={() => (seeking.current = false)}
            onLostPointerCapture={() => (seeking.current = false)}
          >
            <div ref={lanesRef} className="relative flex flex-col gap-1">
              {/* Ruler */}
              <div
                className={`relative ${RULER} ${p.tool === "razor" ? "" : "cursor-pointer"}`}
                aria-hidden="true"
              >
                {minor.map((t) => (
                  <span
                    key={t}
                    className="absolute bottom-0 h-1.5 w-px bg-border-hover"
                    style={{ left: pct(t, d) }}
                  />
                ))}
                {labels.map((t) => (
                  <span
                    key={t}
                    className="app-text-xs absolute top-0.5 font-mono text-text-muted tabular-nums"
                    style={{
                      left: pct(t, d),
                      transform:
                        t === 0
                          ? "none"
                          : t >= d
                            ? "translateX(-100%)"
                            : "translateX(-50%)",
                    }}
                  >
                    {mmss(t)}
                  </span>
                ))}
              </div>
              {EDITOR_TRACKS.map((t, i) => (
                <Lane
                  key={t.id}
                  track={t}
                  doc={doc}
                  flags={p.flags[t.id]}
                  path={paths[i] ?? ""}
                  scene={p.scene}
                  seed={p.seed}
                  timeFromX={timeFromX}
                  onEdit={p.onEdit}
                  onDragStart={p.onDragStart}
                  onDragEnd={p.onDragEnd}
                />
              ))}

              {/* Playhead */}
              <div
                aria-hidden="true"
                className="pointer-events-none absolute inset-y-0 z-30 w-0.5 -translate-x-1/2 bg-text"
                style={{ left: pct(playhead, d) }}
              />
              <div
                role="slider"
                tabIndex={0}
                aria-label="Playhead"
                aria-orientation="horizontal"
                aria-valuemin={0}
                aria-valuemax={d}
                aria-valuenow={playhead}
                aria-valuetext={`${formatClock(playhead)} of ${formatClock(d)}`}
                onKeyDown={(e) => {
                  const delta = arrowDelta(e);
                  if (delta !== null) p.onSeek(playhead + delta);
                  else if (e.key === "Home") p.onSeek(0);
                  else if (e.key === "End") p.onSeek(d);
                  else return;
                  e.preventDefault();
                }}
                className="app-text-xs absolute top-0 z-30 -translate-x-1/2 cursor-ew-resize rounded-md bg-accent px-1.5 py-0.5 font-mono whitespace-nowrap text-accent-foreground tabular-nums"
                style={{ left: pct(playhead, d) }}
              >
                {formatClock(playhead)}
              </div>
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}

export default Timeline;
