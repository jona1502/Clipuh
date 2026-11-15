import type { ElementId } from "@/data/appStatus";
import { dispatch, useApp, type AudioSource } from "../../store";
import { Icon, type IconName } from "../../ui/Icon";
import { Meter } from "../../ui/Meter";
import { PlannedBadge } from "../../ui/PlannedBadge";
import { Slider } from "../../ui/Slider";
import { Card } from "./Card";

const KIND_ICON: Record<AudioSource["kind"], IconName> = {
  game: "gamepad",
  desktop: "monitor",
  mic: "mic",
  extra: "window",
};
const KIND_ID: Partial<Record<AudioSource["kind"], ElementId>> = {
  game: "mixer.gameAudio",
  desktop: "mixer.desktopAudio",
  mic: "mixer.microphone",
};
const GAIN_MIN = -40;
const GAIN_MAX = 0;
const GAIN_STEP = 1;
const dbText = (v: number) => `${v.toFixed(1)} dB`;

const small =
  "app-text-sm inline-flex size-[2.4em] shrink-0 items-center justify-center rounded-md border transition-colors";
const tone = (on: boolean) =>
  on
    ? "border-accent bg-accent-subtle text-text"
    : "border-border bg-surface text-text-secondary hover:bg-surface-hover hover:text-text";

type SourcePatch = Partial<Pick<AudioSource, "gainDb" | "muted" | "solo">>;

function Row({
  source,
  detail,
  anySolo,
}: {
  source: AudioSource;
  detail: string;
  anySolo: boolean;
}) {
  const set = (patch: SourcePatch) =>
    dispatch({ type: "source/set", id: source.id, patch });
  const silent = source.muted || (anySolo && !source.solo);
  const level = silent
    ? 0
    : Math.min(1, ((source.gainDb - GAIN_MIN) / 40) * 0.8);
  const plannedId = KIND_ID[source.kind];
  return (
    <li
      data-mixer-row={source.id}
      className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-x-3 gap-y-2 rounded-lg bg-surface-raised p-3 @min-[60rem]/app-window:grid-cols-[minmax(0,13em)_minmax(0,1fr)_5em_minmax(0,1fr)_auto]"
    >
      <div data-element={plannedId} className="flex min-w-0 items-center gap-3">
        <span className="grid size-[2.8em] shrink-0 place-items-center rounded-lg bg-accent-subtle text-accent">
          <Icon name={KIND_ICON[source.kind]} className="size-[55%]" />
        </span>
        <span className="flex min-w-0 flex-col leading-tight">
          <span className="app-text-base truncate font-semibold">
            {source.label}
            {plannedId && <PlannedBadge id={plannedId} className="ml-2" />}
          </span>
          <span className="app-text-sm truncate text-text-secondary">
            {detail}
          </span>
        </span>
      </div>
      <span
        data-gain-readout={source.id}
        className="app-text-sm text-right text-text-secondary tabular-nums @min-[60rem]/app-window:order-3 @min-[60rem]/app-window:text-center"
      >
        {dbText(source.gainDb)}
      </span>
      <div
        data-element="mixer.meters"
        className="col-span-2 @min-[60rem]/app-window:order-2 @min-[60rem]/app-window:col-span-1"
      >
        <Meter level={level} animated label={`${source.label} level`} />
        <PlannedBadge id="mixer.meters" />
      </div>
      <div
        data-element="mixer.gain"
        className="col-span-2 @min-[60rem]/app-window:order-4 @min-[60rem]/app-window:col-span-1"
      >
        <Slider
          label={`${source.label} gain`}
          min={GAIN_MIN}
          max={GAIN_MAX}
          step={GAIN_STEP}
          value={source.gainDb}
          format={dbText}
          onChange={(v) => set({ gainDb: v })}
          className="w-full"
        />
        <PlannedBadge id="mixer.gain" />
      </div>
      <div className="col-span-2 flex items-center gap-2 @min-[60rem]/app-window:order-5 @min-[60rem]/app-window:col-span-1">
        <span aria-hidden="true" className={`${small} ${tone(!source.muted)}`}>
          <Icon
            name={source.muted ? "volumeMute" : "volume"}
            className="size-[55%]"
          />
        </span>
        <span
          data-element="mixer.solo"
          className="inline-flex items-center gap-1"
        >
          <button
            type="button"
            aria-label={`Solo ${source.label}`}
            aria-pressed={source.solo}
            onClick={() => set({ solo: !source.solo })}
            className={`${small} cursor-pointer ${tone(source.solo)}`}
          >
            S
          </button>
          <PlannedBadge id="mixer.solo" />
        </span>
        <span
          data-element="mixer.mute"
          className="inline-flex items-center gap-1"
        >
          <button
            type="button"
            aria-label={`Mute ${source.label}`}
            aria-pressed={source.muted}
            onClick={() => set({ muted: !source.muted })}
            className={`${small} cursor-pointer ${tone(source.muted)}`}
          >
            M
          </button>
          <PlannedBadge id="mixer.mute" />
        </span>
      </div>
    </li>
  );
}

export function AudioMixer() {
  const sources = useApp((s) => s.sources);
  const devices = useApp((s) => s.settings.devices);
  const anySolo = sources.some((s) => s.solo);
  const detailFor = (s: AudioSource) =>
    s.kind === "mic"
      ? devices.mic
      : s.kind === "desktop"
        ? devices.desktop
        : s.detail;
  return (
    <Card
      icon="headphones"
      title="Audio Mixer"
      subtitle="Manage and control your audio sources."
      action={
        <span
          data-element="mixer.addSource"
          className="inline-flex items-center gap-2"
        >
          <button
            type="button"
            onClick={() => dispatch({ type: "source/add" })}
            className="app-text-sm inline-flex cursor-pointer items-center gap-2 rounded-lg border border-border bg-surface-raised px-3 py-2 whitespace-nowrap text-text transition-colors hover:bg-surface-hover"
          >
            <Icon name="plus" />
            Add Audio Source
          </button>
          <PlannedBadge id="mixer.addSource" />
        </span>
      }
    >
      <ul className="flex flex-col gap-2">
        {sources.map((s) => (
          <Row key={s.id} source={s} detail={detailFor(s)} anySolo={anySolo} />
        ))}
      </ul>
    </Card>
  );
}

export default AudioMixer;
