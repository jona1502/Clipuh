import { useId, type ReactNode } from "react";
import type { ElementId } from "@/data/appStatus";
import { Icon, type IconName } from "../../ui/Icon";
import { PlannedBadge } from "../../ui/PlannedBadge";

/** One settings card (a nav target, found by `data-settings-panel`). */
export function Card({
  id,
  icon,
  title,
  description,
  children,
  className,
}: {
  id: string;
  icon: IconName;
  title: string;
  description: string;
  children: ReactNode;
  className?: string;
}) {
  const titleId = useId();
  return (
    <section
      data-settings-panel={id}
      aria-labelledby={titleId}
      className={`flex min-w-0 flex-col gap-3 rounded-xl border border-border bg-surface p-4${className ? ` ${className}` : ""}`}
    >
      <header className="flex items-center gap-3">
        <span className="grid size-[2.6em] shrink-0 place-items-center rounded-lg border border-border bg-surface-raised text-text">
          <Icon name={icon} className="size-[1.4em]" />
        </span>
        <div className="min-w-0">
          <h4
            id={titleId}
            className="app-text-lg leading-tight font-semibold text-text"
          >
            {title}
          </h4>
          <p className="app-text-sm text-text-secondary">{description}</p>
        </div>
      </header>
      {children}
    </section>
  );
}

/** Label on the left, control on the right. */
export function Row({
  label,
  htmlFor,
  plannedId,
  children,
}: {
  label: string;
  htmlFor?: string;
  plannedId?: ElementId;
  children: ReactNode;
}) {
  return (
    <div
      data-element={plannedId}
      className="app-text-base grid grid-cols-[minmax(0,2fr)_minmax(0,3fr)] items-center gap-3"
    >
      <label htmlFor={htmlFor} className="min-w-0 text-text">
        {label}
      </label>
      <div className="flex min-w-0 flex-wrap items-center gap-1.5">
        {children}
        {plannedId && <PlannedBadge id={plannedId} />}
      </div>
    </div>
  );
}

export function Checkbox({
  label,
  checked,
  onChange,
  plannedId,
  hint,
}: {
  label: string;
  checked: boolean;
  onChange(v: boolean): void;
  plannedId?: ElementId;
  hint: string;
}) {
  return (
    <div
      data-element={plannedId}
      className="app-text-base flex flex-wrap items-center gap-2"
    >
      <label className="flex cursor-pointer items-center gap-2.5 text-text">
        <span className="relative grid size-[1.5em] shrink-0 place-items-center">
          <input
            type="checkbox"
            checked={checked}
            onChange={(e) => onChange(e.target.checked)}
            className="peer size-full cursor-pointer appearance-none rounded-md border border-border-hover bg-surface-raised checked:border-accent checked:bg-accent"
          />
          <Icon
            name="check"
            className="pointer-events-none absolute size-[75%] text-accent-foreground opacity-0 peer-checked:opacity-100"
          />
        </span>
        {label}
      </label>
      <span
        title={hint}
        aria-label={hint}
        role="img"
        className="text-text-muted"
      >
        <Icon name="info" className="size-[1.1em]" />
      </span>
      {plannedId && <PlannedBadge id={plannedId} />}
    </div>
  );
}
