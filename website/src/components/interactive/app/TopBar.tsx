import { CAPTURE_SOURCES, captureSourceById } from "./sources";
import {
  dispatch,
  useApp,
  type Fps,
  type PageId,
  type ReplaySec,
  type Resolution,
} from "./store";
import { Dropdown, type DropdownOption } from "./ui/Dropdown";
import { Icon } from "./ui/Icon";
import { PlannedBadge } from "./ui/PlannedBadge";
import { Toggle } from "./ui/Toggle";

const RESOLUTIONS: DropdownOption<Resolution>[] = (
  ["720p", "1080p", "1440p", "2160p"] as const
).map((v) => ({ value: v, label: v }));
const FPS: DropdownOption<Fps>[] = ([30, 60, 120, 144] as const).map((v) => ({
  value: v,
  label: `${v} FPS`,
}));
const LENGTHS: DropdownOption<ReplaySec>[] = (
  [15, 30, 60, 120, 300] as const
).map((v) => ({ value: v, label: `${v} sec` }));
const SOURCES: DropdownOption<string>[] = CAPTURE_SOURCES.map((s) => ({
  value: s.id,
  label: s.name,
  detail: s.detail,
  icon: s.icon,
}));

const Divider = () => (
  <span
    aria-hidden="true"
    className="hidden h-8 w-px shrink-0 bg-border @min-[60rem]/app-window:block"
  />
);

/**
 * Capture bar shown above every page: source, resolution, FPS, replay
 * length, save hotkey and the replay buffer switch. All values live in the
 * shared store, so every window shows the same bar.
 */
export function TopBar({ navigate }: { navigate(p: PageId): void }) {
  const capture = useApp((s) => s.capture);
  const hotkey = useApp((s) => s.settings.hotkeys.saveClip);
  const set = (patch: Partial<typeof capture>) =>
    dispatch({ type: "capture/set", patch });
  const source = captureSourceById(capture.sourceId);

  return (
    <div
      data-app-shell-chrome="topbar"
      className="grid grid-cols-2 items-center gap-2 rounded-xl border border-border bg-surface p-2 @min-[60rem]/app-window:flex @min-[60rem]/app-window:justify-between @min-[60rem]/app-window:gap-3 @min-[60rem]/app-window:px-3"
    >
      <Dropdown
        label="Capture source"
        value={capture.sourceId}
        options={SOURCES}
        onChange={(v) => set({ sourceId: v })}
        plannedId="topbar.sourcePicker"
        variant="ghost"
        rootClassName="col-span-2 @min-[60rem]/app-window:col-span-1"
        className="w-full gap-3 px-1.5 @min-[60rem]/app-window:min-w-[14em]"
        renderValue={() => (
          <>
            <span className="grid size-[2.4em] shrink-0 place-items-center rounded-lg border border-border bg-surface-raised text-text">
              <Icon name={source.icon} className="size-[1.3em]" />
            </span>
            <span className="flex min-w-0 flex-col leading-tight">
              <span className="app-text-lg truncate font-semibold">
                {source.name}
              </span>
              <span className="app-text-sm truncate text-text-secondary">
                {source.detail}
              </span>
            </span>
          </>
        )}
      />
      <Divider />
      <Dropdown
        label="Resolution"
        value={capture.resolution}
        options={RESOLUTIONS}
        onChange={(v) => set({ resolution: v })}
        plannedId="topbar.resolution"
        variant="ghost"
        rootClassName="w-full @min-[60rem]/app-window:w-auto"
        className="app-text-lg w-full"
      />
      <Divider />
      <Dropdown
        label="Frame rate"
        value={capture.fps}
        options={FPS}
        onChange={(v) => set({ fps: v })}
        plannedId="topbar.fps"
        variant="ghost"
        rootClassName="w-full @min-[60rem]/app-window:w-auto"
        className="app-text-lg w-full"
      />
      <Divider />
      <Dropdown
        label="Replay length"
        value={capture.replaySec}
        options={LENGTHS}
        onChange={(v) => set({ replaySec: v })}
        plannedId="topbar.replayLength"
        variant="ghost"
        align="end"
        rootClassName="w-full @min-[60rem]/app-window:w-auto"
        className="app-text-lg w-full"
      />
      <Divider />
      <span
        className="inline-flex items-center gap-1.5"
        data-element="topbar.hotkey"
      >
        <button
          type="button"
          onClick={() => navigate("settings")}
          aria-label={`Save clip hotkey ${hotkey}, change in Settings`}
          title="Change in Settings"
          className="app-text-base min-h-9 cursor-pointer rounded-md border border-border bg-surface-raised px-3 py-1 font-medium whitespace-nowrap text-text hover:border-border-hover"
        >
          {hotkey}
        </button>
        <PlannedBadge id="topbar.hotkey" />
      </span>
      <Divider />
      <span
        className="col-span-2 inline-flex items-center justify-between gap-3 px-1 @min-[60rem]/app-window:justify-end @min-[60rem]/app-window:px-0"
        data-element="topbar.replayToggle"
      >
        <span
          data-replay-status
          aria-live="polite"
          className={`app-text-lg font-medium whitespace-nowrap ${capture.replayActive ? "text-success" : "text-text-muted"}`}
        >
          {capture.replayActive ? "Replay active" : "Replay paused"}
        </span>
        <Toggle
          label="Replay buffer"
          checked={capture.replayActive}
          onChange={(v) => set({ replayActive: v })}
        />
        <PlannedBadge id="topbar.replayToggle" />
      </span>
    </div>
  );
}

export default TopBar;
