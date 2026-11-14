import { useId, useState, type CSSProperties, type ReactNode } from "react";
import { mixerSources, statusLabel } from "@/data/features";
import { statusBadgeClass, statusDotClass } from "@/components/feature/status";

/**
 * Audio mixer demo. Pure demo state: no getUserMedia, no AudioContext, no
 * permissions. The level bar is a static function of gain and enabled state,
 * captioned "Level preview"; it is not a live meter.
 *
 * Mirrors the app (crates/clipper-ui/src/views/audio.rs): gain is 0-200 % in
 * 50 % steps and "separate tracks" is ONE global switch, not per source.
 */

type SourceId = (typeof mixerSources)[number]["id"];

interface RowState {
  enabled: boolean;
  volume: number;
  open: boolean;
}

const MAX = 200;
const STEP = 50;

const initial: Record<SourceId, RowState> = {
  game: { enabled: true, volume: 100, open: false },
  discord: { enabled: true, volume: 50, open: false },
  desktop: { enabled: true, volume: 100, open: false },
  microphone: { enabled: false, volume: 100, open: false },
};

const DESCRIPTION: Record<SourceId, string> = {
  game: "One application's audio on its own.",
  discord: "Voice chat audio on its own.",
  desktop: "Everything playing on the selected output device.",
  microphone: "Input from the selected microphone.",
};

function Icon({ id, muted }: { id: SourceId; muted: boolean }) {
  let body: ReactNode;
  switch (id) {
    case "game":
      body = (
        <>
          <rect x="3" y="8" width="18" height="9" rx="4.5" />
          <path d="M8 10.5v4M6 12.5h4M16 11.5h.01M18 13.5h.01" />
        </>
      );
      break;
    case "discord":
      body = (
        <>
          <path d="M4 6.5A2.5 2.5 0 0 1 6.5 4h11A2.5 2.5 0 0 1 20 6.5v8a2.5 2.5 0 0 1-2.5 2.5H11l-4 3v-3h-.5A2.5 2.5 0 0 1 4 14.5z" />
          <path d="M9 10h.01M15 10h.01" />
        </>
      );
      break;
    case "desktop":
      body = (
        <>
          <rect x="3" y="4" width="18" height="12" rx="2" />
          <path d="M9 20h6M12 16v4" />
        </>
      );
      break;
    default:
      body = (
        <>
          <rect x="9" y="3" width="6" height="11" rx="3" />
          <path d="M5.5 11a6.5 6.5 0 0 0 13 0M12 17.5V21" />
        </>
      );
  }
  return (
    <svg
      width="20"
      height="20"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.75"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {body}
      {muted && <path d="M4 4l16 16" />}
    </svg>
  );
}

function Chevron({ open }: { open: boolean }) {
  return (
    <svg
      width="16"
      height="16"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      style={{ transform: open ? "rotate(180deg)" : undefined }}
    >
      <path d="M6 9l6 6 6-6" />
    </svg>
  );
}

export default function MixerDemo() {
  const uid = useId();
  const [rows, setRows] = useState(initial);
  const [separate, setSeparate] = useState(true);

  const patch = (id: SourceId, p: Partial<RowState>) =>
    setRows((r) => ({ ...r, [id]: { ...r[id], ...p } }));

  const mixCount = mixerSources.filter((s) => rows[s.id].enabled).length;

  return (
    <div className="flex min-h-[inherit] flex-col">
      <ul className="m-0 list-none p-0">
        {mixerSources.map((s) => {
          const st = rows[s.id];
          const planned = s.status === "planned";
          const detailsId = `${uid}-${s.id}-details`;
          const labelId = `${uid}-${s.id}-label`;
          const stateId = `${uid}-${s.id}-state`;
          const level = st.enabled ? Math.min(st.volume / MAX, 1) * 100 : 0;
          const hot = st.enabled && st.volume > 100;
          return (
            <li
              key={s.id}
              data-source={s.id}
              className="border-b border-border last:border-b-0"
            >
              <div
                className={`grid grid-cols-[minmax(0,1fr)_auto_auto] items-center gap-x-2 px-4 py-1.5 sm:grid-cols-[minmax(0,9.5rem)_auto_minmax(0,1fr)_auto] sm:gap-x-3 ${
                  st.enabled ? "" : "opacity-60"
                }`}
              >
                <div className="flex min-w-0 flex-wrap items-center gap-x-2.5 gap-y-1">
                  <span
                    className={
                      st.enabled ? "text-accent-hover" : "text-text-muted"
                    }
                  >
                    <Icon id={s.id} muted={!st.enabled} />
                  </span>
                  <span id={labelId} className="text-small font-medium">
                    {s.label}
                  </span>
                  {planned && (
                    <span
                      data-status="planned"
                      className={statusBadgeClass("planned", "sm")}
                    >
                      <span
                        className={statusDotClass("planned")}
                        aria-hidden="true"
                      />
                      {statusLabel.planned}
                    </span>
                  )}
                </div>

                <button
                  type="button"
                  className="demo-toggle"
                  aria-pressed={st.enabled}
                  aria-labelledby={`${labelId} ${stateId}`}
                  onClick={() => patch(s.id, { enabled: !st.enabled })}
                >
                  <span id={stateId}>{st.enabled ? "On" : "Off"}</span>
                </button>

                <div className="order-last col-span-3 sm:order-none sm:col-span-1">
                  <div className="flex items-center gap-3">
                    <input
                      type="range"
                      className="demo-slider min-w-0 flex-1"
                      min={0}
                      max={MAX}
                      step={STEP}
                      value={st.volume}
                      disabled={!st.enabled}
                      aria-label={`${s.label} gain`}
                      aria-valuetext={`${st.volume} percent`}
                      style={
                        {
                          "--demo-fill": `${(st.volume / MAX) * 100}%`,
                        } as CSSProperties
                      }
                      onChange={(e) =>
                        patch(s.id, { volume: Number(e.currentTarget.value) })
                      }
                    />
                    <span
                      className="w-11 text-right text-small text-text-secondary tabular-nums"
                      aria-hidden="true"
                    >
                      {st.volume}%
                    </span>
                  </div>
                  <div
                    className="-mt-1 mb-1.5 flex items-center gap-2"
                    aria-hidden="true"
                  >
                    <span className="text-caption text-text-muted">
                      Level preview
                    </span>
                    <div
                      className="h-1 min-w-0 flex-1 overflow-hidden rounded-full bg-surface-hover"
                      data-level={Math.round(level)}
                    >
                      <div
                        className={`h-full rounded-full ${hot ? "bg-warning" : "bg-success"}`}
                        style={{ width: `${level}%` }}
                      />
                    </div>
                  </div>
                </div>

                <button
                  type="button"
                  className="demo-toggle"
                  aria-expanded={st.open}
                  aria-controls={detailsId}
                  aria-label={`${s.label} details`}
                  onClick={() => patch(s.id, { open: !st.open })}
                >
                  <Chevron open={st.open} />
                </button>
              </div>

              <div
                id={detailsId}
                hidden={!st.open}
                className="border-t border-border bg-background-elevated px-4 py-3"
              >
                <p className="m-0 text-small text-text-secondary">
                  {DESCRIPTION[s.id]}
                </p>
                {planned && (
                  <p className="m-0 mt-2 text-small text-text-secondary">
                    Separate per-application capture is planned; today this
                    audio is part of Desktop audio.
                  </p>
                )}
              </div>
            </li>
          );
        })}
      </ul>

      <div className="mt-auto flex flex-wrap items-center justify-between gap-x-4 gap-y-2 border-t border-border px-4 py-3">
        <button
          type="button"
          className="demo-toggle"
          aria-pressed={separate}
          onClick={() => setSeparate((v) => !v)}
        >
          Separate tracks
          <span>{separate ? "On" : "Off"}</span>
        </button>
        <p
          className="m-0 min-w-0 flex-1 basis-56 text-small text-text-secondary"
          role="status"
          aria-atomic="true"
        >
          {`Mix: ${mixCount} of ${mixerSources.length} sources. ${
            separate
              ? "Separate tracks on: each source also gets its own track."
              : "Separate tracks off: one mixed track."
          }`}
        </p>
      </div>
    </div>
  );
}
