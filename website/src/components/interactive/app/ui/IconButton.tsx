import type { ElementId } from "@/data/appStatus";
import { Icon, type IconName } from "./Icon";
import { PlannedBadge } from "./PlannedBadge";

/**
 * Square icon-only button. `label` is the accessible name and tooltip.
 * `pressed` makes it a toggle button (aria-pressed, accent fill).
 * `plannedId` adds a Planned badge after the button.
 */
export function IconButton({
  label,
  icon,
  onClick,
  pressed,
  disabled,
  size = "md",
  plannedId,
  className,
}: {
  label: string;
  icon: IconName;
  onClick?(): void;
  pressed?: boolean;
  disabled?: boolean;
  size?: "sm" | "md";
  plannedId?: ElementId;
  className?: string;
}) {
  const dim = size === "sm" ? "size-7" : "size-9";
  const tone = pressed
    ? "bg-accent-subtle border-accent text-text"
    : "bg-surface-raised border-border text-text-secondary hover:bg-surface-hover hover:text-text";
  const button = (
    <button
      type="button"
      aria-label={label}
      title={label}
      aria-pressed={pressed}
      disabled={disabled}
      onClick={onClick}
      className={`inline-flex ${dim} shrink-0 cursor-pointer items-center justify-center rounded-md border transition-colors disabled:cursor-not-allowed disabled:opacity-50 ${tone}${className ? ` ${className}` : ""}`}
    >
      <Icon name={icon} className="size-[55%]" />
    </button>
  );
  if (!plannedId) return button;
  return (
    <span className="inline-flex items-center gap-1.5" data-element={plannedId}>
      {button}
      <PlannedBadge id={plannedId} />
    </span>
  );
}

export default IconButton;
