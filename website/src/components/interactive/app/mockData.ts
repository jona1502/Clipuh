import type { Clip } from "./store";
import type { ThumbScene } from "./thumbs";

export interface Game {
  id: string;
  name: string;
  scene: ThumbScene;
}

/** Invented titles; deliberately unlike any real game. */
export const GAMES: Game[] = [
  { id: "ridge", name: "Hollow Ridge", scene: "canyon" },
  { id: "neon", name: "Neon Drift", scene: "neon" },
  { id: "grove", name: "Mossgrove", scene: "forest" },
  { id: "keep", name: "Stonekeep", scene: "fortress" },
  { id: "vault", name: "Vault Arena", scene: "arena" },
  { id: "dune", name: "Dune Runners", scene: "desert" },
];

export function gameById(id: string): Game {
  return GAMES.find((g) => g.id === id) ?? GAMES[0];
}

const c = (
  n: number,
  gameId: string,
  title: string,
  minutesAgo: number,
  durationSec: number,
  sizeMb: number,
  kind: Clip["kind"] = "game",
  favorite = false,
): Clip => ({
  id: `clip-${n}`,
  title,
  gameId,
  kind,
  minutesAgo,
  durationSec,
  sizeMb,
  favorite,
});

export const INITIAL_CLIPS: Clip[] = [
  c(1, "ridge", "Hollow Ridge Clip 24", 2, 60, 226),
  c(2, "grove", "Mossgrove Clip 18", 8, 45, 172, "game", true),
  c(3, "keep", "Stonekeep Clip 07", 14, 60, 227),
  c(4, "dune", "Dune Runners Clip 11", 22, 30, 110),
  c(5, "ridge", "Hollow Ridge Clip 23", 36, 60, 229),
  c(6, "vault", "Vault Arena Clip 09", 48, 58, 174),
  c(7, "keep", "Stonekeep Clip 06", 60, 45, 198),
  c(8, "grove", "Mossgrove Clip 17", 75, 60, 236),
  c(9, "neon", "Neon Drift Clip 05", 120, 40, 151, "game", true),
  c(10, "vault", "Vault Arena Clip 08", 190, 52, 168),
  c(11, "neon", "Neon Drift session", 260, 300, 745, "recording"),
  c(12, "dune", "Dune Runners Clip 10", 380, 35, 129),
  c(13, "ridge", "Hollow Ridge ranked night", 1500, 600, 1480, "recording"),
  c(14, "grove", "Mossgrove Clip 16", 1620, 48, 183),
];
