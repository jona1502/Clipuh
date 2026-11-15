import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Clip } from "../store";
import { dispatch, useApp } from "../store";
import { GAMES } from "../mockData";
import type { PageProps } from "../types";
import { Dropdown } from "../ui/Dropdown";
import { Icon } from "../ui/Icon";
import { IconButton } from "../ui/IconButton";
import { ClipCard } from "./library/ClipCard";
import { SelectionBar } from "./library/SelectionBar";
import { UndoToast, type ToastState } from "./library/UndoToast";
import {
  SORTS,
  TABS,
  formatGb,
  selectClips,
  totalMb,
  type LibrarySort,
  type LibraryTab,
  type LibraryView,
} from "./library/logic";

const GAME_FILTERS = [
  { value: "all", label: "All games" },
  ...GAMES.map((g) => ({ value: g.id, label: g.name })),
];

export default function LibraryPage({ navigate, windowRef }: PageProps) {
  const clips = useApp((s) => s.clips);
  const [tab, setTab] = useState<LibraryTab>("all");
  const [query, setQuery] = useState("");
  const [sort, setSort] = useState<LibrarySort>("newest");
  const [game, setGame] = useState("all");
  const [view, setView] = useState<LibraryView>("grid");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [toast, setToast] = useState<ToastState | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const toastId = useRef(0);

  const visible = useMemo(
    () => selectClips(clips, tab, game, query, sort),
    [clips, tab, game, query, sort],
  );
  const chosen = useMemo(
    () => visible.filter((c) => selected.has(c.id)),
    [visible, selected],
  );

  // Ctrl+F focuses search, only while focus is inside this window.
  useEffect(() => {
    const root = windowRef.current;
    if (!root) return;
    const onKey = (e: KeyboardEvent) => {
      if (
        (e.ctrlKey || e.metaKey) &&
        !e.altKey &&
        e.key.toLowerCase() === "f"
      ) {
        e.preventDefault();
        searchRef.current?.focus();
        searchRef.current?.select();
      }
    };
    root.addEventListener("keydown", onKey);
    return () => root.removeEventListener("keydown", onKey);
  }, [windowRef]);

  const dismiss = useCallback(() => setToast(null), []);
  const notify = (message: string, onUndo?: () => void) =>
    setToast({ id: ++toastId.current, message, onUndo });

  const remove = (list: Clip[]) => {
    if (list.length === 0) return;
    dispatch({ type: "clips/delete", ids: list.map((c) => c.id) });
    setSelected(new Set());
    notify(
      `${list.length} ${list.length === 1 ? "clip" : "clips"} deleted`,
      () => dispatch({ type: "clips/restore", clips: list }),
    );
  };
  const openEditor = (id: string) => {
    dispatch({ type: "editor/open", id });
    navigate("editor");
  };
  const toggle = (id: string) =>
    setSelected((prev) => {
      const next = new Set(prev);
      if (!next.delete(id)) next.add(id);
      return next;
    });
  const favoriteSelected = () => {
    const makeFav = chosen.some((c) => !c.favorite);
    chosen
      .filter((c) => c.favorite !== makeFav)
      .forEach((c) => dispatch({ type: "clip/toggleFavorite", id: c.id }));
  };
  const folderNotice = () =>
    notify("Opening folders is not part of this demo.");

  const emptyAll = clips.length === 0;

  return (
    <div className="flex min-h-full flex-col" data-library-page>
      <div className="flex items-center gap-3">
        <Icon name="clapperboard" className="size-7 text-text" />
        <h3 className="app-text-2xl font-semibold text-text">Library</h3>
      </div>

      <div className="mt-3 flex flex-wrap items-center gap-2">
        <div
          role="tablist"
          aria-label="Clip filter"
          className="flex flex-wrap gap-1 rounded-lg border border-border bg-surface p-1"
        >
          {TABS.map((t) => (
            <button
              key={t.id}
              type="button"
              role="tab"
              aria-selected={tab === t.id}
              onClick={() => setTab(t.id)}
              className={`app-text-sm inline-flex min-h-8 cursor-pointer items-center gap-2 rounded-md border px-3 py-1 font-medium transition-colors ${tab === t.id ? "border-accent bg-accent-subtle text-text" : "border-transparent text-text-secondary hover:bg-surface-hover hover:text-text"}`}
            >
              <Icon
                name={
                  t.id === "all"
                    ? "library"
                    : t.id === "favorites"
                      ? "star"
                      : t.id === "game"
                        ? "gamepad"
                        : "record"
                }
                className="size-4"
              />
              {t.label}
            </button>
          ))}
        </div>

        <div className="ml-auto flex flex-wrap items-center gap-2">
          <label className="app-text-sm relative flex min-h-9 min-w-40 items-center gap-2 rounded-md border border-border bg-surface-raised px-3 text-text-secondary focus-within:border-accent">
            <Icon name="search" className="size-4 shrink-0" />
            <input
              ref={searchRef}
              type="search"
              aria-label="Search clips"
              placeholder="Search clips..."
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              className="min-w-0 flex-1 bg-transparent py-1.5 text-text outline-none placeholder:text-text-muted"
            />
            <kbd className="app-text-xs hidden rounded border border-border px-1.5 py-0.5 text-text-muted @min-[48rem]/app-window:block">
              Ctrl + F
            </kbd>
          </label>
          <Dropdown<string>
            label="Filters"
            value={game}
            options={GAME_FILTERS}
            onChange={setGame}
            plannedId="library.filters"
            align="end"
            renderValue={() => (
              <>
                <Icon name="filter" className="size-4" />
                <span>Filters</span>
              </>
            )}
          />
          <Dropdown<LibrarySort>
            label="Sort"
            value={sort}
            options={SORTS}
            onChange={setSort}
            plannedId="library.sort"
            align="end"
            renderValue={(o) => <span>Sort: {o?.label}</span>}
          />
          <div className="flex gap-1">
            <IconButton
              label="Grid view"
              icon="grid"
              pressed={view === "grid"}
              onClick={() => setView("grid")}
              plannedId="library.grid"
            />
            <IconButton
              label="List view"
              icon="list"
              pressed={view === "list"}
              onClick={() => setView("list")}
              plannedId="library.listView"
            />
          </div>
        </div>
      </div>

      <p
        data-library-count
        className="app-text-sm mt-3 mb-2 text-text-secondary"
      >
        {visible.length} {visible.length === 1 ? "clip" : "clips"} •{" "}
        {formatGb(totalMb(visible))}
      </p>

      {visible.length === 0 ? (
        <div className="flex flex-1 flex-col items-center justify-center gap-2 rounded-lg border border-dashed border-border py-10 text-center">
          <Icon name="clapperboard" className="size-10 text-text-muted" />
          <p className="app-text-lg font-semibold text-text">
            {emptyAll ? "No clips yet" : "No clips match"}
          </p>
          <p className="app-text-sm max-w-xs text-text-secondary">
            {emptyAll
              ? "Press the save hotkey while the replay buffer is active to capture your first clip."
              : "Try another search, tab or filter."}
          </p>
        </div>
      ) : (
        <div
          data-library-view={view}
          role="tabpanel"
          aria-label={TABS.find((t) => t.id === tab)?.label}
          className={
            view === "grid"
              ? "grid grid-cols-1 gap-3 @min-[34rem]/app-window:grid-cols-2 @min-[60rem]/app-window:grid-cols-4"
              : "flex flex-col gap-2"
          }
        >
          {visible.map((clip) => (
            <ClipCard
              key={clip.id}
              clip={clip}
              view={view}
              selected={selected.has(clip.id)}
              onSelect={() => toggle(clip.id)}
              onPlay={() => openEditor(clip.id)}
              onEdit={() => openEditor(clip.id)}
              onFavorite={() =>
                dispatch({ type: "clip/toggleFavorite", id: clip.id })
              }
              onDelete={() => remove([clip])}
              onFolder={folderNotice}
              onMore={() => notify("More actions are not part of this demo.")}
            />
          ))}
        </div>
      )}

      <div className="sticky bottom-0 z-10 mt-auto flex flex-col gap-2 pt-3">
        {toast && (
          <div className="flex justify-center">
            <UndoToast toast={toast} onDismiss={dismiss} />
          </div>
        )}
        <SelectionBar
          count={chosen.length}
          totalCount={visible.length}
          sizeMb={totalMb(chosen)}
          onToggleAll={() =>
            setSelected(
              chosen.length === visible.length
                ? new Set()
                : new Set(visible.map((c) => c.id)),
            )
          }
          onDelete={() => remove(chosen)}
          onFavorite={favoriteSelected}
          onFolder={folderNotice}
          onOpenEditor={() => chosen[0] && openEditor(chosen[0].id)}
        />
      </div>
    </div>
  );
}
