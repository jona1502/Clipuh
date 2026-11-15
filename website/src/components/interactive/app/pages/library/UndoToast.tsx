import { useEffect } from "react";

export interface ToastState {
  id: number;
  message: string;
  onUndo?: () => void;
}

/** Bottom toast with an optional Undo. Dismisses itself after a few seconds. */
export function UndoToast({
  toast,
  onDismiss,
}: {
  toast: ToastState;
  onDismiss: () => void;
}) {
  useEffect(() => {
    const t = window.setTimeout(onDismiss, 7000);
    return () => window.clearTimeout(t);
  }, [toast.id, onDismiss]);
  return (
    <div
      data-library-toast
      role="status"
      className="app-text-sm pointer-events-auto flex items-center gap-3 rounded-lg border border-border-hover bg-surface-raised px-4 py-2 text-text shadow-lg"
    >
      <span>{toast.message}</span>
      {toast.onUndo && (
        <button
          type="button"
          onClick={() => {
            toast.onUndo?.();
            onDismiss();
          }}
          className="cursor-pointer rounded-md px-2 py-1 font-semibold text-accent hover:bg-surface-hover"
        >
          Undo
        </button>
      )}
      <button
        type="button"
        aria-label="Dismiss"
        onClick={onDismiss}
        className="cursor-pointer rounded-md px-1.5 py-1 text-text-muted hover:bg-surface-hover hover:text-text"
      >
        ×
      </button>
    </div>
  );
}

export default UndoToast;
