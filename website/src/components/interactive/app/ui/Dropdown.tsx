import {
  useEffect,
  useId,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
} from "react";
import type { ElementId } from "@/data/appStatus";
import { Icon, type IconName } from "./Icon";
import { PlannedBadge } from "./PlannedBadge";

export interface DropdownOption<T extends string | number> {
  value: T;
  label: string;
  /** Second line under the label (e.g. "Game Capture"). */
  detail?: string;
  icon?: IconName;
}

/**
 * Select-only dropdown, APG "collapsible listbox" pattern: a button
 * (aria-haspopup="listbox") opens a `role="listbox"` that takes focus and is
 * driven by Arrow keys, Home/End, Enter/Space (select), Escape/Tab (close).
 *
 * Accessible names: the button is "<label> <current value text>", the
 * listbox is "<label>", options are their `label`. The label itself is not
 * shown; render a visible label next to the dropdown where the design has
 * one.
 *
 * - `variant="field"` (default): bordered input look (settings rows).
 *   `variant="ghost"`: borderless (top bar).
 * - `renderValue` replaces the button content (keep the chevron).
 * - `plannedId` adds a Planned badge after the control.
 * - `className` is applied to the button, `rootClassName` to the wrapper
 *   (use it for grid/flex placement, e.g. `w-full col-span-2`).
 */
export function Dropdown<T extends string | number>({
  label,
  value,
  options,
  onChange,
  plannedId,
  className,
  rootClassName,
  variant = "field",
  align = "start",
  renderValue,
}: {
  label: string;
  value: T;
  options: DropdownOption<T>[];
  onChange(v: T): void;
  plannedId?: ElementId;
  className?: string;
  rootClassName?: string;
  variant?: "field" | "ghost";
  align?: "start" | "end";
  renderValue?(option: DropdownOption<T> | undefined): ReactNode;
}) {
  const id = useId();
  const [open, setOpen] = useState(false);
  const selectedIndex = Math.max(
    0,
    options.findIndex((o) => o.value === value),
  );
  const [active, setActive] = useState(selectedIndex);
  const rootRef = useRef<HTMLSpanElement>(null);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const listRef = useRef<HTMLUListElement>(null);
  const current = options[selectedIndex];

  useEffect(() => {
    if (!open) return;
    listRef.current?.focus();
    const onDown = (e: PointerEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", onDown);
    return () => document.removeEventListener("pointerdown", onDown);
  }, [open]);

  const openAt = (index: number) => {
    setActive(index);
    setOpen(true);
  };
  const close = (refocus: boolean) => {
    setOpen(false);
    if (refocus) buttonRef.current?.focus();
  };
  const choose = (index: number) => {
    const opt = options[index];
    if (opt && opt.value !== value) onChange(opt.value);
    close(true);
  };

  const onButtonKey = (e: KeyboardEvent<HTMLButtonElement>) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      openAt(selectedIndex);
    }
  };
  const onListKey = (e: KeyboardEvent<HTMLUListElement>) => {
    const last = options.length - 1;
    const keys: Record<string, () => void> = {
      ArrowDown: () => setActive((i) => Math.min(last, i + 1)),
      ArrowUp: () => setActive((i) => Math.max(0, i - 1)),
      Home: () => setActive(0),
      End: () => setActive(last),
      Enter: () => choose(active),
      " ": () => choose(active),
      Escape: () => close(true),
    };
    if (e.key === "Tab") {
      setOpen(false);
      return;
    }
    const fn = keys[e.key];
    if (!fn) return;
    e.preventDefault();
    e.stopPropagation();
    fn();
  };

  const base =
    variant === "ghost"
      ? "border-transparent bg-transparent hover:bg-surface-hover"
      : "border-border bg-surface-raised hover:border-border-hover";

  return (
    <span
      ref={rootRef}
      className={`relative inline-flex max-w-full min-w-0 items-center gap-1.5${rootClassName ? ` ${rootClassName}` : ""}`}
      data-element={plannedId}
      onBlur={(e) => {
        if (open && !rootRef.current?.contains(e.relatedTarget as Node))
          setOpen(false);
      }}
    >
      <span id={`${id}-label`} className="sr-only">
        {label}
      </span>
      <button
        ref={buttonRef}
        id={`${id}-button`}
        type="button"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={open ? `${id}-list` : undefined}
        aria-labelledby={`${id}-label ${id}-button`}
        onClick={() => (open ? close(false) : openAt(selectedIndex))}
        onKeyDown={onButtonKey}
        className={`inline-flex min-h-9 min-w-0 cursor-pointer items-center justify-between gap-2 rounded-md border px-3 py-1.5 text-left text-text transition-colors ${base}${className ? ` ${className}` : ""}`}
      >
        <span className="flex min-w-0 items-center gap-2">
          {renderValue ? (
            renderValue(current)
          ) : (
            <>
              {current?.icon && <Icon name={current.icon} />}
              <span className="truncate">{current?.label}</span>
            </>
          )}
        </span>
        <Icon
          name="chevronDown"
          className={`size-[1em] shrink-0 text-text-secondary transition-transform ${open ? "rotate-180" : ""}`}
        />
      </button>
      {plannedId && <PlannedBadge id={plannedId} />}
      {open && (
        <ul
          ref={listRef}
          id={`${id}-list`}
          role="listbox"
          tabIndex={-1}
          aria-label={label}
          aria-activedescendant={`${id}-opt-${active}`}
          onKeyDown={onListKey}
          className={`absolute top-full z-30 mt-1 max-h-64 min-w-full overflow-auto rounded-lg border border-border-hover bg-surface-raised p-1 shadow-lg outline-none ${align === "end" ? "right-0" : "left-0"}`}
        >
          {options.map((opt, i) => {
            const selected = opt.value === value;
            return (
              <li
                key={String(opt.value)}
                id={`${id}-opt-${i}`}
                role="option"
                aria-selected={selected}
                onPointerMove={() => setActive(i)}
                onClick={() => choose(i)}
                className={`flex cursor-pointer items-center gap-2 rounded-md px-2.5 py-1.5 whitespace-nowrap ${i === active ? "bg-surface-hover" : ""} ${selected ? "text-text" : "text-text-secondary"}`}
              >
                {opt.icon && <Icon name={opt.icon} />}
                <span className="flex min-w-0 flex-1 flex-col">
                  <span>{opt.label}</span>
                  {opt.detail && (
                    <span className="app-text-xs text-text-muted">
                      {opt.detail}
                    </span>
                  )}
                </span>
                <Icon
                  name="check"
                  className={`size-[1em] shrink-0 text-accent ${selected ? "" : "invisible"}`}
                />
              </li>
            );
          })}
        </ul>
      )}
    </span>
  );
}

export default Dropdown;
