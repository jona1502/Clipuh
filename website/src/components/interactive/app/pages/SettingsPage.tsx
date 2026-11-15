import { useCallback, useRef, useState } from "react";
import {
  dispatch,
  useApp,
  type Codec,
  type Fps,
  type ReplaySec,
  type Resolution,
  type SettingsState,
} from "../store";
import type { PageProps } from "../types";
import { Dropdown, type DropdownOption } from "../ui/Dropdown";
import { Icon, type IconName } from "../ui/Icon";
import { AppearanceCard } from "./settings/AppearanceCard";
import { HotkeyRow } from "./settings/HotkeyRow";
import { Card, Checkbox, Row } from "./settings/parts";

const opts = <T extends string | number>(
  values: readonly T[],
  label: (v: T) => string,
): DropdownOption<T>[] => values.map((v) => ({ value: v, label: label(v) }));

const RESOLUTIONS: DropdownOption<Resolution>[] = [
  { value: "720p", label: "720p" },
  { value: "1080p", label: "1080p" },
  { value: "1440p", label: "1440p" },
  { value: "2160p", label: "2160p" },
];
const FPS = opts<Fps>([30, 60, 120, 144], (v) => `${v} FPS`);
const ENCODERS = opts(["Hardware (GPU)", "Software (CPU)"], (v) => v);
const BITRATES = opts([8, 12, 20, 35, 50], (v) => `${v} Mbps`);
const CODECS = opts<Codec>(["h264", "hevc", "av1"], (v) =>
  v === "h264" ? "H.264" : v === "hevc" ? "HEVC (H.265)" : "AV1",
);
const GAME_DEVICES = opts(["Game audio", "All applications"], (v) => v);
const DESKTOP_DEVICES = opts(
  ["Default output", "Headphones", "Speakers"],
  (v) => v,
);
const MIC_DEVICES = opts(
  ["Default microphone", "Headset microphone", "None"],
  (v) => v,
);
const DURATIONS = opts<ReplaySec>(
  [15, 30, 60, 120, 300],
  (v) => `${v} seconds`,
);
const BUFFERS: DropdownOption<SettingsState["bufferLocation"]>[] = [
  { value: "ram", label: "RAM" },
  { value: "disk", label: "Disk" },
  { value: "hybrid", label: "RAM + Disk (Hybrid)" },
];
const KEEP: DropdownOption<SettingsState["keepDays"]>[] = [
  { value: 7, label: "7 days" },
  { value: 14, label: "14 days" },
  { value: 30, label: "30 days" },
  { value: 90, label: "90 days" },
  { value: 0, label: "Forever" },
];

const NAV: { id: string; label: string; icon: IconName }[] = [
  { id: "video", label: "Video", icon: "video" },
  { id: "audio", label: "Audio", icon: "volume" },
  { id: "replay", label: "Replay", icon: "replay" },
  { id: "hotkeys", label: "Hotkeys", icon: "keyboard" },
  { id: "storage", label: "Storage", icon: "database" },
  { id: "appearance", label: "Appearance", icon: "palette" },
];

const field = "w-full";
const fieldRoot = "min-w-0 flex-[1_1_9rem]";

export default function SettingsPage({ windowRef }: PageProps) {
  const capture = useApp((s) => s.capture);
  const s = useApp((st) => st.settings);
  const rootRef = useRef<HTMLDivElement>(null);
  const [current, setCurrent] = useState("video");
  const [recording, setRecording] = useState<string | null>(null);
  const setS = (patch: Partial<SettingsState>) =>
    dispatch({ type: "settings/set", patch });
  const setC = (patch: Partial<typeof capture>) =>
    dispatch({ type: "capture/set", patch });
  const recordFor = useCallback(
    (action: string) => (on: boolean) =>
      setRecording((cur) => (on ? action : cur === action ? null : cur)),
    [],
  );
  const recorders = useRef<Record<string, (on: boolean) => void>>({});
  const recorder = (action: string) =>
    (recorders.current[action] ??= recordFor(action));

  /** Scrolls only the page area, never the site. */
  const goTo = useCallback((id: string) => {
    setCurrent(id);
    const root = rootRef.current;
    const panel = root?.querySelector<HTMLElement>(
      `[data-settings-panel="${id}"]`,
    );
    const area = root?.closest<HTMLElement>(".app-page");
    if (!panel || !area || area.scrollHeight <= area.clientHeight) return;
    const reduced = window.matchMedia(
      "(prefers-reduced-motion: reduce)",
    ).matches;
    const top =
      panel.getBoundingClientRect().top -
      area.getBoundingClientRect().top +
      area.scrollTop -
      8;
    area.scrollTo({ top, behavior: reduced ? "auto" : "smooth" });
  }, []);

  const hk = [
    ["saveClip", "Save Clip (Replay)", "settings.hotkeySaveClip"],
    ["recording", "Start / Stop Recording", "settings.hotkeyRecording"],
    ["openApp", "Open Clipuh", "settings.hotkeyOpenApp"],
  ] as const;

  return (
    <div ref={rootRef} className="flex min-w-0 flex-col gap-3">
      <h3 className="app-text-2xl font-semibold">Settings</h3>
      <div className="flex min-w-0 flex-col gap-3 @min-[48rem]/app-window:flex-row">
        <nav
          aria-label="Settings categories"
          className="hidden shrink-0 self-start rounded-xl border border-border bg-surface p-2 @min-[48rem]/app-window:block @min-[48rem]/app-window:w-[clamp(9rem,22%,14rem)]"
        >
          <ul className="flex flex-col gap-1">
            {NAV.map((n) => (
              <li key={n.id}>
                <button
                  type="button"
                  aria-current={current === n.id ? "true" : undefined}
                  onClick={() => goTo(n.id)}
                  className={`app-text-base flex min-h-10 w-full cursor-pointer items-center gap-3 rounded-lg px-3 py-2 text-left ${current === n.id ? "bg-accent text-accent-foreground" : "text-text hover:bg-surface-hover"}`}
                >
                  <Icon name={n.icon} className="size-[1.2em] shrink-0" />
                  {n.label}
                </button>
              </li>
            ))}
          </ul>
        </nav>

        <div className="grid min-w-0 flex-1 grid-cols-1 items-start gap-3 @min-[60rem]/app-window:grid-cols-2">
          <Card
            id="video"
            icon="video"
            title="Video"
            description="Capture quality and encoding settings."
          >
            <Row label="Resolution" plannedId="settings.resolution">
              <Dropdown
                label="Resolution"
                value={capture.resolution}
                options={RESOLUTIONS}
                onChange={(v) => setC({ resolution: v })}
                className={field}
                rootClassName={fieldRoot}
              />
            </Row>
            <Row label="Frame Rate" plannedId="settings.fps">
              <Dropdown
                label="Frame rate"
                value={capture.fps}
                options={FPS}
                onChange={(v) => setC({ fps: v })}
                className={field}
                rootClassName={fieldRoot}
              />
            </Row>
            <Row label="Encoder" plannedId="settings.encoder">
              <Dropdown
                label="Encoder"
                value={s.encoder}
                options={ENCODERS}
                onChange={(v) => setS({ encoder: v })}
                className={field}
                rootClassName={fieldRoot}
              />
            </Row>
            <Row label="Bitrate" plannedId="settings.bitrate">
              <Dropdown
                label="Bitrate"
                value={s.bitrateMbps}
                options={BITRATES}
                onChange={(v) => setS({ bitrateMbps: v })}
                className={field}
                rootClassName={fieldRoot}
              />
            </Row>
            <Row label="Codec">
              <Dropdown
                label="Codec"
                value={capture.codec}
                options={CODECS}
                onChange={(v) => setC({ codec: v })}
                className={field}
                rootClassName={fieldRoot}
              />
            </Row>
          </Card>

          <Card
            id="audio"
            icon="volume"
            title="Audio"
            description="Configure and manage audio capture."
          >
            <Row
              label="Game / Application Audio"
              plannedId="settings.audioDevices"
            >
              <Dropdown
                label="Game audio device"
                value={s.devices.game}
                options={GAME_DEVICES}
                onChange={(v) => setS({ devices: { ...s.devices, game: v } })}
                className={field}
                rootClassName={fieldRoot}
              />
            </Row>
            <Row label="Desktop Audio">
              <Dropdown
                label="Desktop audio device"
                value={s.devices.desktop}
                options={DESKTOP_DEVICES}
                onChange={(v) =>
                  setS({ devices: { ...s.devices, desktop: v } })
                }
                className={field}
                rootClassName={fieldRoot}
              />
            </Row>
            <Row label="Microphone">
              <Dropdown
                label="Microphone device"
                value={s.devices.mic}
                options={MIC_DEVICES}
                onChange={(v) => setS({ devices: { ...s.devices, mic: v } })}
                className={field}
                rootClassName={fieldRoot}
              />
            </Row>
            <Checkbox
              label="Separate audio tracks"
              hint="Save game, desktop and microphone audio as separate tracks."
              checked={s.separateTracks}
              onChange={(v) => setS({ separateTracks: v })}
              plannedId="settings.separateTracks"
            />
          </Card>

          <Card
            id="replay"
            icon="replay"
            title="Replay"
            description="Instant replay and buffer settings."
          >
            <Row label="Replay Duration" plannedId="settings.replayDuration">
              <Dropdown
                label="Replay duration"
                value={capture.replaySec}
                options={DURATIONS}
                onChange={(v) => setC({ replaySec: v })}
                className={field}
                rootClassName={fieldRoot}
              />
            </Row>
            <Row label="Buffer Location" plannedId="settings.bufferLocation">
              <Dropdown
                label="Buffer location"
                value={s.bufferLocation}
                options={BUFFERS}
                onChange={(v) => setS({ bufferLocation: v })}
                className={field}
                rootClassName={fieldRoot}
              />
            </Row>
            <Checkbox
              label="Start replay automatically"
              hint="Start the replay buffer when clipuh launches."
              checked={s.autoStartReplay}
              onChange={(v) => setS({ autoStartReplay: v })}
              plannedId="settings.autoStartReplay"
            />
          </Card>

          <Card
            id="hotkeys"
            icon="keyboard"
            title="Hotkeys"
            description="Global shortcuts for clipping and recording."
          >
            {hk.map(([action, label, plannedId]) => (
              <HotkeyRow
                key={action}
                action={action}
                label={label}
                plannedId={plannedId}
                recording={recording === action}
                onRecord={recorder(action)}
                windowRef={windowRef}
              />
            ))}
          </Card>

          <Card
            id="storage"
            icon="database"
            title="Storage"
            description="Manage disk usage and clip retention."
          >
            <Row
              label="Clip Location"
              htmlFor="settings-clip-location"
              plannedId="settings.clipLocation"
            >
              <span className="relative flex min-w-0 flex-[1_1_9rem] items-center">
                <input
                  id="settings-clip-location"
                  type="text"
                  value={s.clipLocation}
                  onChange={(e) => setS({ clipLocation: e.target.value })}
                  className="app-text-base min-h-9 w-full min-w-0 rounded-md border border-border bg-surface-raised py-1.5 pr-9 pl-3 text-text hover:border-border-hover"
                />
                <Icon
                  name="folder"
                  className="pointer-events-none absolute right-2.5 size-[1.2em] text-text-secondary"
                />
              </span>
            </Row>
            <Row label="Keep Clips For" plannedId="settings.retention">
              <Dropdown
                label="Keep clips for"
                value={s.keepDays}
                options={KEEP}
                onChange={(v) => setS({ keepDays: v })}
                className={field}
                rootClassName={fieldRoot}
              />
            </Row>
            <Row label="Auto Cleanup" plannedId="settings.autoCleanup">
              <Checkbox
                label="Delete old clips automatically"
                hint="Remove clips once they are older than the retention period."
                checked={s.autoCleanup}
                onChange={(v) => setS({ autoCleanup: v })}
              />
            </Row>
          </Card>

          <AppearanceCard />
        </div>
      </div>
    </div>
  );
}
