import type { ElementId } from "@/data/appStatus";

/**
 * Status-tracked elements the shell (top bar + sidebar) renders. Each one is
 * wrapped in `[data-element="<id>"]` with a `<PlannedBadge id>` next to it.
 * Kept React-free so tests can import it.
 */
export const SHELL_ELEMENT_IDS = [
  "topbar.sourcePicker",
  "topbar.resolution",
  "topbar.fps",
  "topbar.replayLength",
  "topbar.hotkey",
  "topbar.replayToggle",
  "sidebar.storageMeter",
] as const satisfies readonly ElementId[];
