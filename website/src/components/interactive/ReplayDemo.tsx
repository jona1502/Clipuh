import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { useReducedMotion } from "./shared/useReducedMotion";

/** Matches the app default (crates/clipper-core/src/hotkey.rs). */
const HOTKEY = ["Ctrl", "Shift", "F10"] as const;
const HOTKEY_TEXT = "Ctrl+Shift+F10";

const DURATIONS = [
  { id: "15", label: "15 s", spoken: "15 seconds", fill: 10 },
  { id: "30", label: "30 s", spoken: "30 seconds", fill: 22 },
  { id: "60", label: "1 min", spoken: "1 minute", fill: 40 },
  { id: "120", label: "2 min", spoken: "2 minutes", fill: 65 },
  { id: "300", label: "5 min", spoken: "5 minutes", fill: 100 },
] as const;

const MAX_CLIPS = 3;
const SAVE_MS = 1000;

interface Clip {
  n: number;
  duration: string;
}

const css = `
@keyframes replay-sheen { from { transform: translateX(-100%); } to { transform: translateX(100%); } }
.replay-sheen { animation: replay-sheen 3.2s linear infinite; }
.replay-sheen[data-paused="true"] { animation-play-state: paused; }
`;

function Keycap({ children }: { children: string }) {
  return (
    <kbd className="inline-flex min-h-8 min-w-8 items-center justify-center rounded-sm border border-b-2 border-border-hover bg-surface-raised px-2 font-mono text-sm font-medium text-text">
      {children}
    </kbd>
  );
}

export default function ReplayDemo() {
  const reduced = useReducedMotion();
  const rootRef = useRef<HTMLDivElement>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const counter = useRef(0);
  const [inView, setInView] = useState(true);
  const [durationId, setDurationId] = useState<string>("30");
  const [saving, setSaving] = useState(false);
  const [clips, setClips] = useState<Clip[]>([]);
  const [announce, setAnnounce] = useState("");

  const duration = DURATIONS.find((d) => d.id === durationId) ?? DURATIONS[1];

  useEffect(() => {
    const el = rootRef.current;
    let io: IntersectionObserver | undefined;
    if (el && "IntersectionObserver" in window) {
      io = new IntersectionObserver(([entry]) =>
        setInView(entry.isIntersecting),
      );
      io.observe(el);
    }
    return () => {
      io?.disconnect();
      if (timer.current) clearTimeout(timer.current);
    };
  }, []);

  function save() {
    if (timer.current) return; // one flow at a time
    setSaving(true);
    setAnnounce("Saving clip…");
    const { spoken, label } = duration;
    timer.current = setTimeout(
      () => {
        timer.current = null;
        counter.current += 1;
        const n = counter.current;
        setClips((prev) =>
          [{ n, duration: label }, ...prev].slice(0, MAX_CLIPS),
        );
        setSaving(false);
        setAnnounce(`Clip saved, last ${spoken}. Demo clip ${n}.`);
      },
      reduced ? 0 : SAVE_MS,
    );
  }

  function onKeyDown(e: KeyboardEvent<HTMLDivElement>) {
    if (e.key === "F10" && e.ctrlKey && e.shiftKey && !e.altKey && !e.metaKey) {
      e.preventDefault();
      save();
    }
  }

  const animate = !reduced && inView;

  return (
    <div
      ref={rootRef}
      onKeyDown={onKeyDown}
      className="flex flex-col gap-4 p-4 text-text sm:p-5"
      role="group"
      aria-label="Instant replay demo"
    >
      <style>{css}</style>

      {/* Placeholder capture scene */}
      <div
        className="relative h-36 overflow-hidden rounded-lg border border-border sm:h-44"
        style={{
          background:
            "radial-gradient(60% 80% at 25% 20%, color-mix(in srgb, var(--color-accent) 35%, transparent), transparent 70%), linear-gradient(160deg, var(--color-surface-raised), var(--color-background-elevated))",
        }}
        aria-hidden="true"
      >
        <div className="absolute bottom-0 left-0 h-1/3 w-2/3 rounded-tr-[3rem] bg-background/50" />
        <div className="absolute top-10 right-8 size-12 rotate-12 rounded-md bg-surface-hover/70" />
        <span className="absolute top-2 left-2 rounded-sm bg-background/70 px-2 py-0.5 text-xs text-text-muted">
          Placeholder scene
        </span>
        <span className="absolute top-2 right-2 inline-flex items-center gap-1.5 rounded-sm bg-background/70 px-2 py-0.5 text-xs text-text-secondary">
          <span className="size-1.5 rounded-full bg-error" />
          Buffering
        </span>
      </div>

      {/* Rolling buffer */}
      <div className="flex flex-col gap-1.5">
        <div className="flex justify-between text-xs text-text-secondary">
          <span>Replay buffer</span>
          <span>Last {duration.label}</span>
        </div>
        <div className="h-2 overflow-hidden rounded-full bg-surface-hover">
          <div
            className="relative h-full overflow-hidden rounded-full bg-accent/80"
            style={{
              width: `${duration.fill}%`,
              transition: reduced ? "none" : "width 250ms var(--ease-out)",
            }}
          >
            {!reduced && (
              <div
                className="replay-sheen absolute inset-0 bg-gradient-to-r from-transparent via-white/30 to-transparent"
                data-paused={!animate}
              />
            )}
          </div>
        </div>
      </div>

      {/* Duration */}
      <fieldset className="m-0 min-w-0 border-0 p-0">
        <legend className="mb-2 p-0 text-xs text-text-secondary">
          Replay length
        </legend>
        <div className="flex flex-wrap gap-2">
          {DURATIONS.map((d) => (
            <label key={d.id} className="relative">
              <input
                type="radio"
                name="replay-duration"
                value={d.id}
                checked={d.id === durationId}
                onChange={() => setDurationId(d.id)}
                className="peer absolute inset-0 m-0 size-full cursor-pointer opacity-0"
              />
              <span className="inline-flex min-h-11 min-w-14 items-center justify-center rounded-md border border-border-hover bg-surface-raised px-3 text-sm font-medium text-text-secondary peer-checked:border-accent peer-checked:bg-accent-subtle peer-checked:text-accent-hover peer-focus-visible:outline-2 peer-focus-visible:outline-offset-2 peer-focus-visible:outline-focus-ring hover:bg-surface-hover">
                {d.label}
              </span>
            </label>
          ))}
        </div>
      </fieldset>

      {/* Hotkey + action */}
      <div className="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-border bg-background-elevated p-3">
        <div className="flex flex-col gap-1.5">
          <div
            className="flex flex-wrap items-center gap-1.5"
            role="img"
            aria-label={`Hotkey ${HOTKEY_TEXT}`}
          >
            {HOTKEY.map((k, i) => (
              <span key={k} className="flex items-center gap-1.5">
                {i > 0 && <span className="text-xs text-text-muted">+</span>}
                <Keycap>{k}</Keycap>
              </span>
            ))}
          </div>
          <span className="text-xs text-text-muted">
            Default hotkey, customizable
          </span>
        </div>
        <button
          type="button"
          onClick={save}
          aria-disabled={saving}
          className="inline-flex min-h-11 items-center justify-center rounded-md bg-accent px-4 text-sm font-semibold text-accent-foreground hover:bg-accent-hover focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus-ring aria-disabled:opacity-70"
        >
          {saving ? "Saving…" : `Press ${HOTKEY_TEXT}`}
        </button>
      </div>

      {/* Clips */}
      <ul
        className="m-0 flex min-h-[8.5rem] list-none flex-col gap-2 p-0"
        aria-label="Saved demo clips"
      >
        {clips.length === 0 && (
          <li className="rounded-md border border-dashed border-border px-3 py-3 text-sm text-text-muted">
            No clips yet. Press the hotkey or the button.
          </li>
        )}
        {clips.map((c) => (
          <li
            key={c.n}
            className="flex items-center gap-3 rounded-md border border-border bg-surface-raised px-3 py-2"
          >
            <span
              className="relative h-8 w-12 shrink-0 rounded-sm"
              style={{
                background:
                  "linear-gradient(135deg, color-mix(in srgb, var(--color-accent) 40%, transparent), var(--color-surface-hover))",
              }}
              aria-hidden="true"
            />
            <span className="flex min-w-0 flex-col">
              <span className="truncate text-sm font-medium text-text">
                Demo clip {c.n}
              </span>
              <span className="text-xs text-text-secondary">
                Clip saved · {c.duration}
              </span>
            </span>
          </li>
        ))}
      </ul>

      <p className="m-0 text-xs text-text-muted">
        With focus inside this demo, {HOTKEY_TEXT} also works.
      </p>

      <div aria-live="polite" role="status" className="sr-only">
        {announce}
      </div>
    </div>
  );
}
