import type { Clip } from "../../store";
import { gameById } from "../../mockData";

export type LibraryTab = "all" | "favorites" | "game" | "recording";
export type LibrarySort = "newest" | "oldest" | "largest" | "longest";
export type LibraryView = "grid" | "list";

export const TABS: { id: LibraryTab; label: string }[] = [
  { id: "all", label: "All Clips" },
  { id: "favorites", label: "Favorites" },
  { id: "game", label: "Game Clips" },
  { id: "recording", label: "Recordings" },
];

export const SORTS: { value: LibrarySort; label: string }[] = [
  { value: "newest", label: "Newest" },
  { value: "oldest", label: "Oldest" },
  { value: "largest", label: "Largest" },
  { value: "longest", label: "Longest" },
];

const sorters: Record<LibrarySort, (a: Clip, b: Clip) => number> = {
  newest: (a, b) => a.minutesAgo - b.minutesAgo,
  oldest: (a, b) => b.minutesAgo - a.minutesAgo,
  largest: (a, b) => b.sizeMb - a.sizeMb,
  longest: (a, b) => b.durationSec - a.durationSec,
};

const tabFilters: Record<LibraryTab, (c: Clip) => boolean> = {
  all: () => true,
  favorites: (c) => c.favorite,
  game: (c) => c.kind === "game",
  recording: (c) => c.kind === "recording",
};

/** Tab, game filter ("all" or a game id) and search, then sorted. */
export function selectClips(
  clips: Clip[],
  tab: LibraryTab,
  game: string,
  query: string,
  sort: LibrarySort,
): Clip[] {
  const q = query.trim().toLowerCase();
  return clips
    .filter(tabFilters[tab])
    .filter((c) => game === "all" || c.gameId === game)
    .filter(
      (c) =>
        !q ||
        c.title.toLowerCase().includes(q) ||
        gameById(c.gameId).name.toLowerCase().includes(q),
    )
    .sort(sorters[sort]);
}

export const totalMb = (clips: Clip[]) =>
  clips.reduce((sum, c) => sum + c.sizeMb, 0);

export const formatGb = (mb: number) => `${(mb / 1024).toFixed(1)} GB`;

export function formatAgo(minutes: number): string {
  if (minutes < 60) return `${minutes} min ago`;
  if (minutes < 1440) return `${Math.floor(minutes / 60)} h ago`;
  return `${Math.floor(minutes / 1440)} d ago`;
}

export function formatDuration(sec: number): string {
  const m = Math.floor(sec / 60);
  const s = sec % 60;
  return `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
}

/** Stable numeric seed for the generated thumbnail. */
export const thumbSeed = (id: string) => Number(id.replace(/\D/g, "")) || 1;
