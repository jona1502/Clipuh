import { useEffect, useRef, type RefObject } from "react";
import type { ElementId } from "@/data/appStatus";
import { appStore, dispatch, useApp, type HotkeyAction } from "../../store";
import { Icon } from "../../ui/Icon";
import { PlannedBadge } from "../../ui/PlannedBadge";

/** "Mod + Key" combo from a key event, or null while only modifiers are down. */
export function comboFromEvent(e: KeyboardEvent): string | null {
  let key: string;
  if (/^Key[A-Z]$/.test(e.code)) key = e.code.slice(3);
  else if (/^Digit\d$/.test(e.code)) key = e.code.slice(5);
  else if (["Control", "Alt", "Shift", "Meta", "AltGraph"].includes(e.key))
    return null;
  else key = e.key.length === 1 ? e.key.toUpperCase() : e.key;
  if (key === " ") key = "Space";
  const isFn = /^F\d{1,2}$/.test(key);
  const mods = [
    e.ctrlKey && "Ctrl",
    e.altKey && "Alt",
    e.shiftKey && "Shift",
    e.metaKey && "Win",
  ].filter(Boolean) as string[];
  // A global shortcut needs a modifier (or a function key).
  if (mods.length === 0 && !isFn) return null;
  return [...mods, key].join(" + ");
}

/**
 * One hotkey row. While recording, key presses inside the window build a
 * combo; Escape or leaving the button cancels. A clash keeps recording and
 * shows the store error until recording ends.
 */
export function HotkeyRow({
  action,
  label,
  plannedId,
  recording,
  onRecord,
  windowRef,
}: {
  action: HotkeyAction;
  label: string;
  plannedId: ElementId;
  recording: boolean;
  onRecord(on: boolean): void;
  windowRef: RefObject<HTMLDivElement | null>;
}) {
  const combo = useApp((s) => s.settings.hotkeys[action]);
  const error = useApp((s) => s.lastError);
  const btn = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    const root = windowRef.current;
    if (!recording || !root) return;
    const onKey = (e: KeyboardEvent) => {
      // Bare Tab / Shift+Tab is never recorded: let it move focus so keyboard
      // users can leave the row (onBlur then ends recording).
      if (e.key === "Tab" && !e.ctrlKey && !e.altKey && !e.metaKey) return;
      e.preventDefault();
      e.stopPropagation();
      if (e.key === "Escape") {
        dispatch({ type: "error/clear" });
        onRecord(false);
        btn.current?.focus();
        return;
      }
      const next = comboFromEvent(e);
      if (!next) return;
      dispatch({ type: "hotkey/set", action, combo: next });
      if (!appStore.getState().lastError) onRecord(false);
    };
    root.addEventListener("keydown", onKey, true);
    return () => {
      root.removeEventListener("keydown", onKey, true);
      dispatch({ type: "error/clear" });
    };
  }, [recording, action, onRecord, windowRef]);

  return (
    <div
      data-hotkey-row={action}
      data-element={plannedId}
      className="app-text-base grid grid-cols-[minmax(0,2fr)_minmax(0,3fr)] items-center gap-3"
    >
      <span className="min-w-0 text-text">{label}</span>
      <div className="flex min-w-0 flex-wrap items-center gap-1.5">
        <button
          ref={btn}
          type="button"
          aria-label={`Change ${label} hotkey, currently ${combo}`}
          aria-pressed={recording}
          onClick={() => onRecord(!recording)}
          onBlur={() => recording && onRecord(false)}
          className={`flex min-h-9 min-w-0 flex-[1_1_9rem] cursor-pointer items-center justify-between gap-2 rounded-md border bg-surface-raised px-3 py-1.5 text-left text-text ${recording ? "border-accent" : "border-border hover:border-border-hover"}`}
        >
          <span className="truncate">{combo}</span>
          <Icon
            name="pencil"
            className="size-[1.1em] shrink-0 text-text-secondary"
          />
        </button>
        <PlannedBadge id={plannedId} />
      </div>
      {recording && !error && (
        <p className="app-text-sm col-span-2 text-text-secondary">
          Press a shortcut, Esc to cancel.
        </p>
      )}
      {recording && error && (
        <p role="alert" className="app-text-sm col-span-2 text-error">
          {error}
        </p>
      )}
    </div>
  );
}
