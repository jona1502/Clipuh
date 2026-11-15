import { Icon, type IconName } from "../../ui/Icon";

function BarButton({
  label,
  icon,
  onClick,
  disabled,
  primary,
}: {
  label: string;
  icon: IconName;
  onClick(): void;
  disabled?: boolean;
  primary?: boolean;
}) {
  const tone = primary
    ? "bg-accent text-accent-foreground border-transparent hover:bg-accent-hover"
    : "bg-surface-raised text-text border-border hover:bg-surface-hover";
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className={`app-text-sm inline-flex min-h-9 cursor-pointer items-center gap-2 rounded-md border px-3 py-1.5 font-medium transition-colors disabled:cursor-not-allowed disabled:opacity-50 ${tone}`}
    >
      <Icon name={icon} className="size-4" />
      {label}
    </button>
  );
}

/** Bottom action bar: select all, summary and bulk actions. */
export function SelectionBar({
  count,
  totalCount,
  sizeMb,
  onToggleAll,
  onDelete,
  onFavorite,
  onFolder,
  onOpenEditor,
}: {
  count: number;
  totalCount: number;
  sizeMb: number;
  onToggleAll(): void;
  onDelete(): void;
  onFavorite(): void;
  onFolder(): void;
  onOpenEditor(): void;
}) {
  const none = count === 0;
  const all = totalCount > 0 && count === totalCount;
  return (
    <div
      role="toolbar"
      aria-label="Selection actions"
      className="flex flex-wrap items-center gap-2 rounded-lg border border-border bg-surface px-3 py-2"
    >
      <button
        type="button"
        role="checkbox"
        aria-checked={all ? true : count > 0 ? "mixed" : false}
        aria-label="Select all clips"
        onClick={onToggleAll}
        disabled={totalCount === 0}
        className="inline-flex size-7 cursor-pointer items-center justify-center rounded-md text-text hover:bg-surface-hover disabled:cursor-not-allowed disabled:opacity-50"
      >
        <Icon
          name={all ? "checkboxChecked" : "checkbox"}
          className={`size-5 ${all ? "text-accent" : ""}`}
        />
      </button>
      <span
        data-selection-summary
        className="app-text-base mr-auto text-text-secondary"
      >
        {count} selected • {sizeMb} MB
      </span>
      <BarButton
        label="Delete"
        icon="trash"
        onClick={onDelete}
        disabled={none}
      />
      <BarButton
        label="Favorite"
        icon="star"
        onClick={onFavorite}
        disabled={none}
      />
      <BarButton
        label="Open Folder"
        icon="folderOpen"
        onClick={onFolder}
        disabled={none}
      />
      <BarButton
        label="Open in Editor"
        icon="editor"
        primary
        onClick={onOpenEditor}
        disabled={count !== 1}
      />
    </div>
  );
}

export default SelectionBar;
