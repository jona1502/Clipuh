import { useCallback, useEffect, useReducer, useRef, useState } from "react";
import { useReducedMotion } from "../../shared/useReducedMotion";
import { clamp } from "../../editor/model";
import { gameById } from "../mockData";
import { dispatch, useApp, type Clip } from "../store";
import type { PageProps } from "../types";
import { EditorHeader } from "./editor/EditorHeader";
import {
  deleteAt,
  historyReducer,
  initialHistory,
  resetTrim,
  setEdge,
  split,
  type Doc,
  type TrackId,
} from "./editor/editorState";
import { Preview, type PreviewLook } from "./editor/Preview";
import { SidePanel, type ExportPhase, type SideTab } from "./editor/SidePanel";
import { Timeline, type Tool } from "./editor/Timeline";
import { NO_FLAGS, type TrackFlags } from "./editor/tracks";

const TICK_MS = 80;
const EXPORT_TICK_MS = 110;
const START_PLAYHEAD = 124;
const LOCKED_MSG = "The video track is locked.";

/** Keys typed into these stay with the control (Space, Ctrl+Z). */
const INTERACTIVE =
  "input, textarea, select, button, a[href], [contenteditable], [role=slider], [role=tab], [role=option], [role=menuitem], [role=listbox]";

const allFlags = (): Record<TrackId, TrackFlags> => ({
  video: { ...NO_FLAGS },
  game: { ...NO_FLAGS },
  desktop: { ...NO_FLAGS },
  mic: { ...NO_FLAGS },
});

function EditorBody({ clip, windowRef }: PageProps & { clip: Clip }) {
  const reduced = useReducedMotion();
  const duration = Math.max(10, clip.durationSec * 10);
  const [hist, send] = useReducer(historyReducer, duration, initialHistory);
  const doc = hist.present;
  const trim = doc.trims.video;

  const [playhead, setPlayhead] = useState(Math.min(START_PLAYHEAD, duration));
  const [playing, setPlaying] = useState(false);
  const [flags, setFlags] = useState(allFlags);
  const [tab, setTab] = useState<SideTab>("trim");
  const [zoom, setZoom] = useState(1);
  const [tool, setTool] = useState<Tool>("select");
  const [snap, setSnap] = useState(false);
  const [link, setLink] = useState(true);
  const [look, setLook] = useState<PreviewLook>({
    boost: false,
    vignette: false,
    overlay: "",
  });
  const [gains, setGains] = useState<Record<TrackId, number>>({
    video: 0,
    game: -6,
    desktop: -12,
    mic: -3,
  });
  const [volume, setVolume] = useState(80);
  const [previewMuted, setPreviewMuted] = useState(false);
  const [exp, setExp] = useState<{ phase: ExportPhase; pct: number }>({
    phase: "idle",
    pct: 0,
  });
  const [toast, setToast] = useState("");

  const rootRef = useRef<HTMLDivElement>(null);
  const playheadRef = useRef(playhead);
  const trimEndRef = useRef(trim.end);
  useEffect(() => {
    playheadRef.current = playhead;
    trimEndRef.current = trim.end;
  }, [playhead, trim.end]);

  const notify = useCallback((msg: string) => setToast(msg), []);
  useEffect(() => {
    if (!toast) return;
    const t = window.setTimeout(() => setToast(""), 2600);
    return () => window.clearTimeout(t);
  }, [toast]);

  // ---- edits (undoable) -----------------------------------------------
  const edit = useCallback(
    (fn: (d: Doc) => Doc) => send({ type: "edit", fn }),
    [],
  );
  const dragStart = useCallback(() => send({ type: "dragStart" }), []);
  const dragEnd = useCallback(() => send({ type: "dragEnd" }), []);
  const undo = useCallback(() => send({ type: "undo" }), []);
  const redo = useCallback(() => send({ type: "redo" }), []);

  // Any edit invalidates a running or finished export.
  useEffect(() => {
    setExp((e) => (e.phase === "idle" ? e : { phase: "idle", pct: 0 }));
  }, [doc]);

  // ---- playback -----------------------------------------------------------
  const seek = useCallback(
    (t: number) => {
      setPlaying(false);
      setPlayhead(clamp(Math.round(t), 0, duration));
    },
    [duration],
  );

  const togglePlay = useCallback(() => {
    if (reduced) {
      notify("Playback is off with reduced motion. Use the ±10 s buttons.");
      return;
    }
    if (playing) {
      setPlaying(false);
      return;
    }
    const t = playheadRef.current;
    if (t < trim.start || t >= trim.end - 1) {
      playheadRef.current = trim.start;
      setPlayhead(trim.start);
    }
    setPlaying(true);
  }, [reduced, notify, playing, trim.start, trim.end]);

  useEffect(() => {
    if (!playing || reduced) return;
    const from = playheadRef.current;
    const t0 = performance.now();
    const id = window.setInterval(() => {
      const next = Math.floor(from + (performance.now() - t0) / 100);
      const end = trimEndRef.current;
      if (next >= end) {
        setPlayhead(end);
        setPlaying(false);
      } else setPlayhead(next);
    }, TICK_MS);
    return () => window.clearInterval(id);
  }, [playing, reduced]);

  // Pause when the window scrolls out of view or the tab is hidden.
  useEffect(() => {
    if (!playing) return;
    const root = rootRef.current;
    const stop = () => setPlaying(false);
    const onVis = () => {
      if (document.hidden) stop();
    };
    const io =
      root && "IntersectionObserver" in window
        ? new IntersectionObserver(([e]) => {
            if (e && !e.isIntersecting) stop();
          })
        : null;
    if (root) io?.observe(root);
    document.addEventListener("visibilitychange", onVis);
    return () => {
      io?.disconnect();
      document.removeEventListener("visibilitychange", onVis);
    };
  }, [playing]);

  // ---- window-scoped shortcuts: Space, Ctrl+Z, Ctrl+Shift+Z / Ctrl+Y ----
  useEffect(() => {
    const win = windowRef.current;
    if (!win) return;
    const onKey = (e: KeyboardEvent) => {
      if ((e.target as HTMLElement | null)?.closest(INTERACTIVE)) return;
      const mod = e.ctrlKey || e.metaKey;
      const key = e.key.toLowerCase();
      if (e.key === " " && !mod && !e.altKey) togglePlay();
      else if (mod && key === "z") (e.shiftKey ? redo : undo)();
      else if (mod && key === "y") redo();
      else return;
      e.preventDefault();
    };
    win.addEventListener("keydown", onKey);
    return () => win.removeEventListener("keydown", onKey);
  }, [windowRef, togglePlay, undo, redo]);

  // ---- export (simulated, nothing is written) -----------------------------
  useEffect(() => {
    if (exp.phase !== "exporting") return;
    if (reduced) {
      const t = window.setTimeout(
        () => setExp({ phase: "done", pct: 100 }),
        400,
      );
      return () => window.clearTimeout(t);
    }
    const id = window.setInterval(() => {
      setExp((e) => {
        const pct = Math.min(100, e.pct + 8);
        return { phase: pct >= 100 ? "done" : "exporting", pct };
      });
    }, EXPORT_TICK_MS);
    return () => window.clearInterval(id);
  }, [exp.phase, reduced]);

  const runExport = useCallback(() => {
    setExp((e) =>
      e.phase === "exporting" ? e : { phase: "exporting", pct: 0 },
    );
  }, []);

  // ---- actions -------------------------------------------------------------
  const onFlag = useCallback((id: TrackId, key: keyof TrackFlags) => {
    setFlags((f) => ({ ...f, [id]: { ...f[id], [key]: !f[id][key] } }));
  }, []);

  const onSplitAt = (t: number) => {
    const next = split(doc, t);
    if (next === doc) notify("Can't split here. Move the playhead first.");
    else edit(() => next);
  };
  const onDelete = () => {
    if (flags.video.locked) return notify(LOCKED_MSG);
    const next = deleteAt(doc, playhead);
    if (next === doc)
      notify("Split first, then delete the segment under the playhead.");
    else edit(() => next);
  };
  const onRename = (title: string) => {
    const t = title.trim();
    if (!t) {
      notify("A clip title can't be empty. Kept the old title.");
      return;
    }
    if (t !== clip.title)
      dispatch({ type: "clip/rename", id: clip.id, title: t });
  };

  const scene = gameById(clip.gameId).scene;
  const seed = Number(clip.id.replace(/\D/g, "")) || 1;
  const outside = playhead < trim.start || playhead > trim.end;
  const savedText =
    hist.past.length > 0
      ? "Unsaved edits (sample)"
      : `Saved ${clip.minutesAgo} min ago`;

  return (
    <div
      ref={rootRef}
      data-editor-page
      tabIndex={-1}
      className="relative flex flex-col gap-3 outline-none @min-[60rem]/app-window:h-full @min-[60rem]/app-window:gap-2"
    >
      <EditorHeader
        title={clip.title}
        savedText={savedText}
        exportPhase={exp.phase}
        exportPct={exp.pct}
        onRename={onRename}
        onExport={runExport}
      />
      <div className="grid gap-3 @min-[48rem]/app-window:grid-cols-[minmax(0,1.7fr)_minmax(0,1fr)] @min-[60rem]/app-window:min-h-0 @min-[60rem]/app-window:flex-1 @min-[60rem]/app-window:grid-rows-[minmax(0,1fr)] @min-[60rem]/app-window:gap-2">
        <Preview
          scene={scene}
          seed={seed}
          playhead={playhead}
          duration={duration}
          playing={playing}
          outside={outside}
          videoHidden={flags.video.hidden}
          look={look}
          volume={volume}
          previewMuted={previewMuted}
          onTogglePlay={togglePlay}
          onSeek={seek}
          onSkip={(delta) => seek(playhead + delta)}
          onToStart={() => seek(trim.start)}
          onToEnd={() => seek(trim.end)}
          onVolume={(v) => {
            setVolume(v);
            setPreviewMuted(v === 0);
          }}
          onToggleMute={() => setPreviewMuted((m) => !m)}
          onFullscreen={() =>
            notify("Fullscreen preview is not part of the demo.")
          }
        />
        <SidePanel
          tab={tab}
          onTab={setTab}
          scene={scene}
          seed={seed}
          trim={trim}
          duration={duration}
          locked={flags.video.locked}
          onSetEdge={(edge, v) => edit((d) => setEdge(d, "video", edge, v))}
          onStepFrame={(delta) => seek(playhead + delta)}
          onSplit={() =>
            flags.video.locked ? notify(LOCKED_MSG) : onSplitAt(playhead)
          }
          onDelete={onDelete}
          onFreeze={() => notify("Freeze frame is planned.")}
          look={look}
          onLook={(patch) => setLook((l) => ({ ...l, ...patch }))}
          flags={flags}
          onFlag={onFlag}
          gains={gains}
          onGain={(id, db) => setGains((g) => ({ ...g, [id]: db }))}
          exportPhase={exp.phase}
          onExport={runExport}
        />
      </div>
      <div className="shrink-0">
        <Timeline
          doc={doc}
          playhead={playhead}
          scene={scene}
          seed={seed}
          flags={flags}
          onFlag={onFlag}
          zoom={zoom}
          onZoom={setZoom}
          tool={tool}
          onTool={setTool}
          snap={snap}
          onSnap={() => {
            setSnap((s) => !s);
            notify("Snapping is planned.");
          }}
          link={link}
          onLink={() => {
            setLink((l) => !l);
            notify("Linking audio to video is planned.");
          }}
          canUndo={hist.past.length > 0}
          canRedo={hist.future.length > 0}
          onUndo={undo}
          onRedo={redo}
          onEdit={edit}
          onDragStart={dragStart}
          onDragEnd={dragEnd}
          onSeek={seek}
          onSplitAt={onSplitAt}
          onResetTrim={(id) => edit((d) => resetTrim(d, id))}
          notify={notify}
        />
      </div>
      <p
        role="status"
        className={`app-text-sm pointer-events-none absolute bottom-3 left-1/2 z-50 max-w-[90%] -translate-x-1/2 rounded-md border border-border-hover bg-surface-raised px-3 py-1.5 text-center text-text shadow-md transition-opacity ${toast ? "opacity-100" : "opacity-0"}`}
      >
        {toast}
      </p>
    </div>
  );
}

/** Editor page: the clip open in the shared store, edited locally. */
export default function EditorPage(props: PageProps) {
  const clip = useApp(
    (s) => s.clips.find((c) => c.id === s.editorClipId) ?? null,
  );
  if (!clip) {
    return (
      <div className="flex h-full min-h-48 flex-col items-center justify-center gap-3 rounded-xl border border-border bg-surface p-6 text-center">
        <p className="app-text-base text-text">No clip is open.</p>
        <button
          type="button"
          onClick={() => props.navigate("library")}
          className="app-text-sm cursor-pointer rounded-md bg-accent px-3 py-1.5 font-semibold text-accent-foreground hover:bg-accent-hover"
        >
          Open the Library
        </button>
      </div>
    );
  }
  return <EditorBody key={clip.id} clip={clip} {...props} />;
}
