import { useId, useState, type CSSProperties, type ReactNode } from "react";
import { mixerSources, statusLabel } from "@/data/features";

/**
 * Audio mixer demo. Pure demo state: no getUserMedia, no AudioContext, no
 * permissions. The level bar is a static function of volume and enabled state.
 */

type SourceId = (typeof mixerSources)[number]["id"];

interface RowState {
  enabled: boolean;
  volume: number;
  separate: boolean;
  open: boolean;
}

const MAX = 150;

const initial: Record<SourceId, RowState> = {
  game: { enabled: true, volume: 100, separate: true, open: false },
  discord: { enabled: true, volume: 80, separate: true, open: false },
  desktop: { enabled: true, volume: 100, separate: false, open: false },
  microphone: { enabled: false, volume: 100, separate: false, open: false },
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

  const patch = (id: SourceId, p: Partial<RowState>) =>
    setRows((r) => ({ ...r, [id]: { ...r[id], ...p } }));

  const mixCount = mixerSources.filter((s) => rows[s.id].enabled).length;
  const trackCount = mixerSources.filter((s) => rows[s.id].separate).length;

  return (
    <div className="flex min-h-[inherit] flex-col">
      <ul className="m-0 list-none p-0">
        {mixerSources.map((s) => {
          const st = rows[s.id];
          const planned = s.status === "planned";
          const detailsId = `${uid}-${s.id}-details`;
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
                  <span className="text-small font-medium">{s.label}</span>
                  {planned && (
                    <span
                      data-status="planned"
                      className="inline-flex items-center gap-1.5 rounded-full border border-border-hover bg-transparent px-2 py-0.5 text-caption font-semibold whitespace-nowrap text-text-secondary"
                    >
                      <span
                        className="size-1.5 rounded-full border border-text-muted"
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
                  aria-label={`Enable ${s.label}`}
                  onClick={() => patch(s.id, { enabled: !st.enabled })}
                >
                  {st.enabled ? "On" : "Off"}
                </button>

                <div className="order-last col-span-3 sm:order-none sm:col-span-1">
                  <div className="flex items-center gap-3">
                    <input
                      type="range"
                      className="demo-slider min-w-0 flex-1"
                      min={0}
                      max={MAX}
                      step={1}
                      value={st.volume}
                      disabled={!st.enabled}
                      aria-label={`${s.label} volume`}
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
                    className="-mt-1 mb-1.5 h-1 overflow-hidden rounded-full bg-surface-hover"
                    aria-hidden="true"
                    data-level={Math.round(level)}
                  >
                    <div
                      className={`h-full rounded-full ${hot ? "bg-warning" : "bg-success"}`}
                      style={{ width: `${level}%` }}
                    />
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
                <button
                  type="button"
                  className="demo-toggle"
                  aria-pressed={st.separate}
                  disabled={!st.enabled}
                  onClick={() => patch(s.id, { separate: !st.separate })}
                >
                  Recorded on its own track
                  <span className="text-text-muted">
                    {st.separate ? "· on" : "· off"}
                  </span>
                </button>
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

      <p
        className="m-0 mt-auto flex flex-wrap justify-between gap-x-4 gap-y-1 border-t border-border px-4 py-3 text-small text-text-secondary"
        aria-live="polite"
      >
        <span>
          Mix: {mixCount} of {mixerSources.length} sources
        </span>
        <span>Separate tracks: {trackCount}</span>
      </p>
    </div>
  );
}
