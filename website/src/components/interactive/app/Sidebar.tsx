import { withBase } from "@/lib/url";
import { useApp, type PageId } from "./store";
import { PAGE_LABELS } from "./types";
import { Icon, type IconName } from "./ui/Icon";
import { PlannedBadge } from "./ui/PlannedBadge";

const NAV: { id: PageId; icon: IconName }[] = [
  { id: "capture", icon: "capture" },
  { id: "library", icon: "library" },
  { id: "editor", icon: "editor" },
  { id: "settings", icon: "settings" },
];

function NavButtons({
  page,
  onNavigate,
  compact,
}: {
  page: PageId;
  onNavigate(p: PageId): void;
  compact: boolean;
}) {
  return NAV.map(({ id, icon }) => {
    const active = id === page;
    return (
      <li key={id} className={compact ? "flex-1" : undefined}>
        <button
          type="button"
          aria-current={active ? "page" : undefined}
          onClick={() => onNavigate(id)}
          className={`flex w-full cursor-pointer items-center rounded-lg font-medium transition-colors ${
            compact
              ? "app-text-sm min-h-11 flex-col justify-center gap-0.5 px-1 py-1"
              : "app-text-lg min-h-11 gap-3 px-3 py-2"
          } ${
            active
              ? "bg-accent text-accent-foreground"
              : "text-text-secondary hover:bg-surface-hover hover:text-text"
          }`}
        >
          <Icon name={icon} className="size-[1.25em] shrink-0" />
          {PAGE_LABELS[id]}
        </button>
      </li>
    );
  });
}

/**
 * App sidebar: logo, the four pages, storage meter. Below the 48rem window
 * width it renders as a top tab row instead (no logo or meter). The unused
 * copy is display:none, so only one nav landmark per window is exposed.
 */
export function Sidebar({
  page,
  onNavigate,
  label,
}: {
  page: PageId;
  onNavigate(p: PageId): void;
  /** Landmark name; unique per window (e.g. "Capture window pages"). */
  label: string;
}) {
  const { usedGb, totalGb } = useApp((s) => s.storage);
  const pct = Math.round((usedGb / totalGb) * 100);
  return (
    <aside className="app-sidebar" data-app-shell-chrome="sidebar">
      {/* Phone: tab row. */}
      <nav aria-label={label} className="app-sidebar-mobile p-1.5">
        <ul className="flex gap-1">
          <NavButtons page={page} onNavigate={onNavigate} compact />
        </ul>
      </nav>
      {/* From 48rem: column. */}
      <div className="app-sidebar-desktop h-full flex-col gap-6 px-3 pt-4 pb-5">
        <div className="flex items-center gap-2 px-2 text-text">
          <img
            src={withBase("clipuh-logo-128.png")}
            alt=""
            width="28"
            height="28"
            className="size-[1.6em] shrink-0"
          />
          <span className="app-text-2xl font-bold tracking-tight">clipuh</span>
        </div>
        <nav aria-label={label}>
          <ul className="flex flex-col gap-1.5">
            <NavButtons page={page} onNavigate={onNavigate} compact={false} />
          </ul>
        </nav>
        <div
          className="app-text-sm mt-auto flex flex-col gap-2 px-1"
          data-element="sidebar.storageMeter"
        >
          <div className="flex items-center justify-between gap-2">
            <span className="flex items-center gap-2 text-text">
              <Icon name="drive" />
              Storage
            </span>
            <span className="whitespace-nowrap text-text-secondary">
              {(totalGb - usedGb).toFixed(1)} GB free
            </span>
          </div>
          <div
            role="meter"
            aria-label="Storage used"
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={pct}
            aria-valuetext={`${usedGb.toFixed(1)} of ${totalGb} GB used`}
            className="h-2 overflow-hidden rounded-full bg-surface-hover"
          >
            <div
              className="h-full rounded-full bg-accent"
              style={{ width: `${pct}%` }}
            />
          </div>
          <span className="text-text-muted">
            {usedGb.toFixed(1)} GB / {totalGb} GB
          </span>
          <PlannedBadge id="sidebar.storageMeter" />
        </div>
      </div>
    </aside>
  );
}

export default Sidebar;
