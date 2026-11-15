import {
  useId,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
} from "react";
import type { ElementId } from "@/data/appStatus";
import { estimateClipMb, useApp } from "../../store";
import { ClipThumb, type ThumbScene } from "../../thumbs";
import { Dropdown, type DropdownOption } from "../../ui/Dropdown";
import { Icon, type IconName } from "../../ui/Icon";
import { IconButton } from "../../ui/IconButton";
import { PlannedBadge } from "../../ui/PlannedBadge";
import { Slider } from "../../ui/Slider";
import { BIG_STEP, STEP } from "../../../editor/model";
import {
  formatClock,
  parseClock,
  type Range,
  type TrackId,
} from "./editorState";
import type { PreviewLook } from "./Preview";
import { EDITOR_TRACKS, type TrackFlags } from "./tracks";

export type SideTab = "trim" | "effects" | "audio" | "text" | "export";
export type ExportPhase = "idle" | "exporting" | "done";

const TABS: { id: SideTab; label: string; planned?: ElementId }[] = [
  { id: "trim", label: "Trim", planned: "editor.trim" },
  { id: "effects", label: "Effects", planned: "editor.tabEffects" },
  { id: "audio", label: "Audio", planned: "editor.tabAudio" },
  { id: "text", label: "Text", planned: "editor.tabText" },
  { id: "export", label: "Export", planned: "editor.export" },
];

interface Props {
  tab: SideTab;
  onTab(t: SideTab): void;
  scene: ThumbScene;
  seed: number;
  trim: Range;
  duration: number;
  locked: boolean;
  onSetEdge(edge: "start" | "end", v: number): void;
  onStepFrame(delta: number): void;
  onSplit(): void;
  onDelete(): void;
  onFreeze(): void;
  look: PreviewLook;
  onLook(patch: Partial<PreviewLook>): void;
  flags: Record<TrackId, TrackFlags>;
  onFlag(id: TrackId, key: keyof TrackFlags): void;
  gains: Record<TrackId, number>;
  onGain(id: TrackId, db: number): void;
  exportPhase: ExportPhase;
  onExport(): void;
}

/** Clock text field: commits on Enter/blur, Arrow Up/Down step 0.1 s. */
function TimeField({
  label,
  value,
  onCommit,
  disabled,
}: {
  label: string;
  value: number;
  onCommit?(v: number): void;
  disabled?: boolean;
}) {
  const id = useId();
  const [draft, setDraft] = useState<string | null>(null);
  const readOnly = !onCommit;
  const commit = () => {
    if (draft === null) return;
    const v = parseClock(draft);
    setDraft(null);
    if (v !== null && onCommit) onCommit(v);
  };
  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (readOnly) return;
    if (e.key === "Enter") commit();
    else if (e.key === "Escape") {
      if (draft === null) return;
      e.stopPropagation();
      setDraft(null);
    } else if (e.key === "ArrowUp" || e.key === "ArrowDown") {
      e.preventDefault();
      const step = e.shiftKey ? BIG_STEP : STEP;
      setDraft(null);
      onCommit?.(value + (e.key === "ArrowUp" ? step : -step));
    }
  };
  return (
    <>
      <label htmlFor={id} className="app-text-sm text-text-secondary">
        {label}
      </label>
      <div className="relative">
        <input
          id={id}
          type="text"
          inputMode="decimal"
          spellCheck={false}
          autoComplete="off"
          readOnly={readOnly}
          disabled={disabled}
          value={draft ?? formatClock(value)}
          onChange={(e) => setDraft(e.currentTarget.value)}
          onBlur={commit}
          onKeyDown={onKeyDown}
          className={`app-text-sm w-full rounded-md px-2 py-1 font-mono text-text tabular-nums disabled:opacity-50 ${readOnly ? "bg-transparent" : "border border-border bg-surface-raised pr-7 hover:border-border-hover"}`}
        />
        {!readOnly && (
          <Icon
            name="chevronUpDown"
            className="pointer-events-none absolute top-1/2 right-2 size-3.5 -translate-y-1/2 text-text-muted"
          />
        )}
      </div>
    </>
  );
}

function ActionButton({
  icon,
  label,
  onClick,
  planned,
}: {
  icon: IconName;
  label: string;
  onClick(): void;
  planned: ElementId;
}) {
  return (
    <span data-element={planned} className="relative flex min-w-0 flex-col">
      <button
        type="button"
        onClick={onClick}
        className="app-text-sm inline-flex cursor-pointer items-center justify-center gap-1.5 rounded-md border border-border bg-surface-raised px-2 py-1.5 text-text hover:bg-surface-hover"
      >
        <Icon name={icon} className="size-4" />
        {label}
      </button>
      <PlannedBadge
        id={planned}
        className="pointer-events-none absolute -top-2.5 right-1"
      />
    </span>
  );
}

function TrimPanel(p: Props) {
  const len = p.trim.end - p.trim.start;
  return (
    <div className="flex flex-col gap-2.5">
      <div className="grid gap-3 @min-[22rem]:grid-cols-[minmax(0,1.15fr)_minmax(0,1fr)] @min-[48rem]/app-window:grid-cols-[minmax(0,1.15fr)_minmax(0,1fr)]">
        <div>
          <h4 className="app-text-base mb-1.5 font-semibold text-text @min-[60rem]/app-window:sr-only">
            Clip
          </h4>
          <div className="grid grid-cols-[3.25rem_minmax(0,1fr)] items-center gap-x-2 gap-y-0.5">
            <TimeField
              label="Start"
              value={p.trim.start}
              disabled={p.locked}
              onCommit={(v) => p.onSetEdge("start", v)}
            />
            <TimeField
              label="End"
              value={p.trim.end}
              disabled={p.locked}
              onCommit={(v) => p.onSetEdge("end", v)}
            />
            <TimeField label="Length" value={len} />
          </div>
        </div>
        <div className="flex w-full max-w-64 items-center self-center justify-self-center rounded-lg border border-border bg-surface-raised p-1.5">
          <IconButton
            label="Previous frame"
            icon="chevronLeft"
            size="sm"
            onClick={() => p.onStepFrame(-STEP)}
          />
          <div className="relative mx-1 aspect-video min-w-0 flex-1 overflow-hidden rounded-md border-2 border-accent">
            <ClipThumb
              scene={p.scene}
              seed={p.seed}
              className="pointer-events-none h-full w-full"
            />
          </div>
          <IconButton
            label="Next frame"
            icon="chevronRight"
            size="sm"
            onClick={() => p.onStepFrame(STEP)}
          />
        </div>
      </div>
      <div className="grid grid-cols-3 gap-2">
        <ActionButton
          icon="split"
          label="Split"
          onClick={p.onSplit}
          planned="editor.split"
        />
        <ActionButton
          icon="trash"
          label="Delete"
          onClick={p.onDelete}
          planned="editor.deleteSegment"
        />
        <ActionButton
          icon="snowflake"
          label="Freeze"
          onClick={p.onFreeze}
          planned="editor.freeze"
        />
      </div>
    </div>
  );
}

function Chip({
  label,
  pressed,
  onClick,
}: {
  label: string;
  pressed: boolean;
  onClick(): void;
}) {
  return (
    <button
      type="button"
      aria-pressed={pressed}
      onClick={onClick}
      className={`app-text-sm cursor-pointer rounded-md border px-3 py-2 text-left transition-colors ${pressed ? "border-accent bg-accent-subtle text-text" : "border-border bg-surface-raised text-text-secondary hover:bg-surface-hover hover:text-text"}`}
    >
      {label}
    </button>
  );
}

function EffectsPanel(p: Props) {
  return (
    <div className="flex flex-col gap-3">
      <p className="app-text-sm text-text-secondary">
        Effects are planned. Here they only restyle the sample preview.
      </p>
      <div className="grid grid-cols-2 gap-2">
        <Chip
          label="Color boost"
          pressed={p.look.boost}
          onClick={() => p.onLook({ boost: !p.look.boost })}
        />
        <Chip
          label="Vignette"
          pressed={p.look.vignette}
          onClick={() => p.onLook({ vignette: !p.look.vignette })}
        />
      </div>
    </div>
  );
}

function AudioPanel(p: Props) {
  return (
    <ul className="flex flex-col gap-2">
      {EDITOR_TRACKS.filter((t) => t.wave).map((t) => (
        <li
          key={t.id}
          className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-x-2 gap-y-1"
        >
          <span className="app-text-sm truncate text-text">{t.name}</span>
          <span className="app-text-xs font-mono text-text-muted tabular-nums">
            {p.gains[t.id] > 0 ? "+" : ""}
            {p.gains[t.id]} dB
          </span>
          <Slider
            label={`${t.name} gain`}
            min={-30}
            max={6}
            step={1}
            value={p.gains[t.id]}
            onChange={(v) => p.onGain(t.id, v)}
            format={(v) => `${v} dB`}
            disabled={p.flags[t.id].muted}
            className="w-full"
          />
          <IconButton
            label={`Mute ${t.name} (panel)`}
            icon={p.flags[t.id].muted ? "volumeMute" : "volume"}
            size="sm"
            pressed={p.flags[t.id].muted}
            onClick={() => p.onFlag(t.id, "muted")}
          />
        </li>
      ))}
    </ul>
  );
}

function TextPanel(p: Props) {
  const id = useId();
  return (
    <div className="flex flex-col gap-2">
      <label htmlFor={id} className="app-text-sm text-text-secondary">
        Overlay text
      </label>
      <input
        id={id}
        type="text"
        maxLength={40}
        placeholder="Type a caption"
        value={p.look.overlay}
        onChange={(e) => p.onLook({ overlay: e.currentTarget.value })}
        className="app-text-sm rounded-md border border-border bg-surface-raised px-2.5 py-1.5 text-text placeholder:text-text-muted"
      />
      <p className="app-text-xs text-text-muted">
        Text overlays are planned. The caption only shows on this preview.
      </p>
    </div>
  );
}

type ExportRes = "source" | "720p";
const RESOLUTIONS: DropdownOption<ExportRes>[] = [
  { value: "source", label: "Source (1080p)" },
  { value: "720p", label: "720p" },
];

function ExportPanel(p: Props) {
  const bitrate = useApp((s) => s.settings.bitrateMbps);
  const [res, setRes] = useState<ExportRes>("source");
  const lenSec = (p.trim.end - p.trim.start) / 10;
  const mb = estimateClipMb(lenSec, bitrate);
  return (
    <div className="flex flex-col gap-3">
      <div className="grid grid-cols-[5.5rem_minmax(0,1fr)] items-center gap-2">
        <span className="app-text-sm text-text-secondary">Format</span>
        <span className="app-text-sm text-text">MP4 (H.264)</span>
        <span className="app-text-sm text-text-secondary">Resolution</span>
        <Dropdown
          label="Export resolution"
          value={res}
          options={RESOLUTIONS}
          onChange={(v) => setRes(v)}
        />
        <span className="app-text-sm text-text-secondary">Range</span>
        <span className="app-text-sm font-mono text-text tabular-nums">
          {formatClock(p.trim.start)} – {formatClock(p.trim.end)}
        </span>
        <span className="app-text-sm text-text-secondary">Estimate</span>
        <span className="app-text-sm text-text">About {mb} MB</span>
      </div>
      <button
        type="button"
        onClick={p.onExport}
        disabled={p.exportPhase === "exporting"}
        className="app-text-sm inline-flex cursor-pointer items-center justify-center gap-2 rounded-md bg-accent px-3 py-2 font-semibold text-accent-foreground hover:bg-accent-hover disabled:cursor-progress disabled:opacity-60"
      >
        <Icon name="upload" className="size-4" />
        Start export
      </button>
    </div>
  );
}

const PANELS: Record<SideTab, (p: Props) => ReactNode> = {
  trim: TrimPanel,
  effects: EffectsPanel,
  audio: AudioPanel,
  text: TextPanel,
  export: ExportPanel,
};

/** Side panel: Trim / Effects / Audio / Text / Export tabs (APG tabs). */
export function SidePanel(p: Props) {
  const base = useId();
  const tabRefs = useRef<Partial<Record<SideTab, HTMLButtonElement | null>>>(
    {},
  );
  const onTabKey = (e: KeyboardEvent<HTMLButtonElement>, i: number) => {
    let j: number;
    if (e.key === "ArrowRight") j = (i + 1) % TABS.length;
    else if (e.key === "ArrowLeft") j = (i - 1 + TABS.length) % TABS.length;
    else if (e.key === "Home") j = 0;
    else if (e.key === "End") j = TABS.length - 1;
    else return;
    e.preventDefault();
    const next = TABS[j]!.id;
    p.onTab(next);
    tabRefs.current[next]?.focus();
  };
  return (
    <section
      aria-label="Clip settings"
      className="@container flex min-h-0 min-w-0 flex-col gap-2 rounded-xl border border-border bg-surface p-2"
    >
      <div
        role="tablist"
        aria-label="Editor panels"
        className="flex gap-1 overflow-x-auto rounded-lg border border-border bg-surface-raised p-1 @min-[48rem]/app-window:grid @min-[48rem]/app-window:grid-cols-5 @min-[48rem]/app-window:overflow-visible"
      >
        {TABS.map((t, i) => {
          const selected = p.tab === t.id;
          return (
            <button
              key={t.id}
              ref={(el) => {
                tabRefs.current[t.id] = el;
              }}
              type="button"
              role="tab"
              id={`${base}-tab-${t.id}`}
              aria-selected={selected}
              aria-controls={`${base}-panel-${t.id}`}
              tabIndex={selected ? 0 : -1}
              onClick={() => p.onTab(t.id)}
              onKeyDown={(e) => onTabKey(e, i)}
              data-element={t.planned}
              className={`app-text-sm flex min-w-max flex-1 cursor-pointer flex-col items-center justify-center gap-0.5 rounded-md px-1.5 py-1 transition-colors @min-[48rem]/app-window:min-w-0 ${selected ? "bg-accent text-accent-foreground" : "text-text-secondary hover:bg-surface-hover hover:text-text"}`}
            >
              <span className="max-w-full truncate">{t.label}</span>
              {t.planned && <PlannedBadge id={t.planned} />}
            </button>
          );
        })}
      </div>
      {TABS.map((t) => {
        const Panel = PANELS[t.id];
        return (
          <div
            key={t.id}
            role="tabpanel"
            id={`${base}-panel-${t.id}`}
            aria-label={t.label}
            hidden={p.tab !== t.id}
            className="min-h-0 flex-1 overflow-y-auto"
          >
            {p.tab === t.id && <Panel {...p} />}
          </div>
        );
      })}
    </section>
  );
}

export default SidePanel;
