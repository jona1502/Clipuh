import type { RefObject } from "react";
import type { PageId } from "./store";

/** Props every page component (pages/*Page.tsx) receives from AppShell. */
export interface PageProps {
  /** Switch this window to another page (never scrolls the site). */
  navigate: (p: PageId) => void;
  /** The window root. Attach page keyboard handlers here (or check
   *  `windowRef.current.contains(e.target)`), never on `document`. */
  windowRef: RefObject<HTMLDivElement | null>;
}

export const PAGE_LABELS: Record<PageId, string> = {
  capture: "Capture",
  library: "Library",
  editor: "Editor",
  settings: "Settings",
};
