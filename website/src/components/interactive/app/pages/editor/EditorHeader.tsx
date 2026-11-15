import { useEffect, useRef, useState } from "react";
import { Icon } from "../../ui/Icon";
import { PlannedBadge } from "../../ui/PlannedBadge";
import type { ExportPhase } from "./SidePanel";

interface Props {
  title: string;
  savedText: string;
  exportPhase: ExportPhase;
  exportPct: number;
  onRename(title: string): void;
  onExport(): void;
}

/** Title row: clip name with rename, save state, export with progress. */
export function EditorHeader(p: Props) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(p.title);
  const inputRef = useRef<HTMLInputElement>(null);
  const pencilRef = useRef<HTMLButtonElement>(null);
  const done = useRef(false);

  useEffect(() => {
    if (editing) inputRef.current?.select();
  }, [editing]);

  const start = () => {
    done.current = false;
    setDraft(p.title);
    setEditing(true);
  };
  const finish = (save: boolean) => {
    if (done.current) return;
    done.current = true;
    if (save) p.onRename(draft);
    setEditing(false);
    requestAnimationFrame(() => pencilRef.current?.focus());
  };

  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-2 rounded-xl border border-border bg-surface px-3 py-1">
      <div className="flex min-w-0 flex-1 basis-56 items-center gap-2">
        {editing ? (
          <input
            ref={inputRef}
            aria-label="Clip title"
            value={draft}
            maxLength={60}
            onChange={(e) => setDraft(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") finish(true);
              else if (e.key === "Escape") {
                e.stopPropagation();
                finish(false);
              }
            }}
            onBlur={() => finish(true)}
            className="app-text-lg min-w-0 flex-1 rounded-md border border-accent bg-surface-raised px-2 py-0.5 font-semibold text-text"
          />
        ) : (
          <h3
            data-editor-title
            className="app-text-lg min-w-0 truncate font-semibold text-text"
          >
            {p.title}
          </h3>
        )}
        <span
          className="inline-flex shrink-0 items-center gap-1.5"
          data-element="editor.rename"
        >
          {!editing && (
            <button
              ref={pencilRef}
              type="button"
              aria-label="Rename clip"
              title="Rename clip"
              onClick={start}
              className="grid size-7 cursor-pointer place-items-center rounded-md text-text-secondary hover:bg-surface-hover hover:text-text"
            >
              <Icon name="pencil" className="size-4" />
            </button>
          )}
          <PlannedBadge id="editor.rename" />
        </span>
      </div>

      <div className="flex flex-wrap items-center gap-3">
        <div className="app-text-sm flex min-h-7 items-center gap-2 text-text-secondary">
          {p.exportPhase === "idle" ? (
            <Icon name="checkCircle" className="size-4" />
          ) : (
            <span
              role="progressbar"
              aria-label="Export progress"
              aria-valuemin={0}
              aria-valuemax={100}
              aria-valuenow={p.exportPct}
              className="relative block h-1.5 w-24 overflow-hidden rounded-full bg-surface-raised"
            >
              <span
                className="absolute inset-y-0 left-0 rounded-full bg-accent"
                style={{ width: `${p.exportPct}%` }}
              />
            </span>
          )}
          <span
            role="status"
            className={`font-mono tabular-nums ${p.exportPhase === "done" ? "font-sans text-success" : p.exportPhase === "idle" ? "font-sans" : ""}`}
          >
            {p.exportPhase === "idle"
              ? p.savedText
              : p.exportPhase === "done"
                ? "Exported (sample)"
                : `Exporting… ${p.exportPct}%`}
          </span>
        </div>
        <span data-element="editor.export" className="inline-flex">
          <button
            type="button"
            aria-label="Export clip"
            onClick={p.onExport}
            disabled={p.exportPhase === "exporting"}
            className="app-text-sm inline-flex cursor-pointer items-center gap-2 rounded-md bg-accent px-3 py-1.5 font-semibold text-accent-foreground hover:bg-accent-hover disabled:cursor-progress disabled:opacity-60"
          >
            <Icon name="upload" className="size-4" />
            Export
          </button>
        </span>
      </div>
    </div>
  );
}

export default EditorHeader;
