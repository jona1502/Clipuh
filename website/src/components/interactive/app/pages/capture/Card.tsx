import type { ReactNode } from "react";
import { Icon, type IconName } from "../../ui/Icon";

/** Titled panel used by every Capture page section. */
export function Card({
  icon,
  title,
  subtitle,
  action,
  className,
  children,
  ...rest
}: {
  icon: IconName;
  title: string;
  subtitle: string;
  action?: ReactNode;
  className?: string;
  children: ReactNode;
} & Record<`data-${string}`, string | undefined>) {
  return (
    <section
      className={`flex min-w-0 flex-col gap-3 rounded-xl border border-border bg-surface p-4${className ? ` ${className}` : ""}`}
      {...rest}
    >
      <header className="flex flex-wrap items-center gap-3">
        <span className="grid size-[2.8em] shrink-0 place-items-center rounded-lg bg-accent-subtle text-accent">
          <Icon name={icon} className="size-[55%]" />
        </span>
        <div className="min-w-[8em] flex-1">
          <h3 className="app-text-lg truncate font-semibold">{title}</h3>
          <p className="app-text-sm truncate text-text-secondary">{subtitle}</p>
        </div>
        {action}
      </header>
      {children}
    </section>
  );
}

export default Card;
