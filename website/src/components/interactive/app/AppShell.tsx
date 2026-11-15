import {
  Suspense,
  lazy,
  useCallback,
  useEffect,
  useRef,
  useState,
  type ComponentType,
  type CSSProperties,
} from "react";
import { Sidebar } from "./Sidebar";
import { TopBar } from "./TopBar";
import { ACCENTS, useApp, type PageId } from "./store";
import { PAGE_LABELS, type PageProps } from "./types";
import { Icon, type IconName } from "./ui/Icon";

// Each page is its own chunk, fetched when a window first shows it.
const PAGES = {
  capture: lazy(() => import("./pages/CapturePage")),
  library: lazy(() => import("./pages/LibraryPage")),
  editor: lazy(() => import("./pages/EditorPage")),
  settings: lazy(() => import("./pages/SettingsPage")),
} satisfies Record<PageId, ComponentType<PageProps>>;

/** Same footprint as the page area it replaces (see .app-page-skeleton). */
function PageSkeleton() {
  return (
    <div className="app-page-skeleton" aria-hidden="true">
      <div />
      <div />
      <div />
    </div>
  );
}

function useResolvedTheme(theme: "dark" | "light" | "system") {
  const [prefersLight, setPrefersLight] = useState(false);
  useEffect(() => {
    if (theme !== "system") return;
    const mql = window.matchMedia("(prefers-color-scheme: light)");
    setPrefersLight(mql.matches);
    const on = (e: MediaQueryListEvent) => setPrefersLight(e.matches);
    mql.addEventListener("change", on);
    return () => mql.removeEventListener("change", on);
  }, [theme]);
  if (theme === "system") return prefersLight ? "light" : "dark";
  return theme;
}

const CHROME: { icon: IconName; label: string }[] = [
  { icon: "minimize", label: "Minimize" },
  { icon: "maximize", label: "Maximize" },
  { icon: "close", label: "Close" },
];

/**
 * The app window: sidebar, window controls, capture bar and the current
 * page. Each window keeps its own page; data and settings come from the
 * shared store, so every window on the site stays in sync.
 */
export function AppShell({
  initialPage,
  windowLabel,
}: {
  initialPage: PageId;
  /** Names the window in landmarks; defaults to the initial page label. */
  windowLabel?: string;
}) {
  const [page, setPage] = useState<PageId>(initialPage);
  const [notice, setNotice] = useState("");
  const windowRef = useRef<HTMLDivElement>(null);
  const accent = useApp((s) => s.settings.accent);
  const theme = useResolvedTheme(useApp((s) => s.settings.theme));
  const navigate = useCallback((p: PageId) => setPage(p), []);

  useEffect(() => {
    if (!notice) return;
    const t = window.setTimeout(() => setNotice(""), 2500);
    return () => window.clearTimeout(t);
  }, [notice]);

  const Page = PAGES[page];
  const style = {
    "--color-accent": `var(${ACCENTS[accent].tokenVar})`,
  } as CSSProperties;

  return (
    <div
      ref={windowRef}
      data-app-window
      data-page={page}
      data-theme={theme}
      data-accent={accent}
      style={style}
      className="relative w-full overflow-hidden"
    >
      <div className="app-layout">
        <Sidebar
          page={page}
          onNavigate={navigate}
          label={`${windowLabel ?? PAGE_LABELS[initialPage]} window pages`}
        />
        <div className="app-main">
          <div
            className="-mb-1 hidden justify-end gap-1 @min-[48rem]/app-window:flex"
            data-app-shell-chrome="titlebar"
          >
            {CHROME.map(({ icon, label }) => (
              <button
                key={icon}
                type="button"
                aria-label={`${label} (demo window)`}
                onClick={() =>
                  setNotice("Window controls are not part of the demo.")
                }
                className="grid size-8 cursor-pointer place-items-center rounded-md text-text-secondary hover:bg-surface-hover hover:text-text"
              >
                <Icon name={icon} className="size-4" />
              </button>
            ))}
          </div>
          <TopBar navigate={navigate} />
          <div className="app-page">
            <Suspense fallback={<PageSkeleton />}>
              <Page navigate={navigate} windowRef={windowRef} />
            </Suspense>
          </div>
        </div>
      </div>
      <p
        role="status"
        className={`app-text-sm pointer-events-none absolute bottom-3 left-1/2 -translate-x-1/2 rounded-md border border-border-hover bg-surface-raised px-3 py-1.5 text-text shadow-md transition-opacity ${notice ? "opacity-100" : "opacity-0"}`}
      >
        {notice}
      </p>
    </div>
  );
}

export default AppShell;
