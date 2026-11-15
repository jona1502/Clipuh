import type { Clip } from "../../store";
import { gameById } from "../../mockData";
import { ClipThumb } from "../../thumbs";
import { Icon, type IconName } from "../../ui/Icon";
import {
  formatAgo,
  formatDuration,
  thumbSeed,
  type LibraryView,
} from "./logic";

function Action({
  label,
  icon,
  onClick,
  pressed,
  primary,
}: {
  label: string;
  icon: IconName;
  onClick(): void;
  pressed?: boolean;
  primary?: boolean;
}) {
  const tone = primary
    ? "bg-accent text-accent-foreground hover:bg-accent-hover"
    : pressed
      ? "bg-accent-subtle text-accent border-accent"
      : "bg-surface-raised text-text-secondary hover:bg-surface-hover hover:text-text";
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      aria-pressed={pressed}
      onClick={onClick}
      className={`inline-flex h-8 min-w-0 flex-1 cursor-pointer items-center justify-center rounded-md border border-border transition-colors ${tone}`}
    >
      <Icon name={icon} className={`size-4 ${pressed ? "fill-current" : ""}`} />
    </button>
  );
}

export interface ClipCardProps {
  clip: Clip;
  view: LibraryView;
  selected: boolean;
  onSelect(): void;
  onPlay(): void;
  onEdit(): void;
  onFavorite(): void;
  onDelete(): void;
  onFolder(): void;
  onMore(): void;
}

export function ClipCard(p: ClipCardProps) {
  const { clip } = p;
  const game = gameById(clip.gameId);
  const list = p.view === "list";
  return (
    <article
      data-clip-id={clip.id}
      data-selected={p.selected}
      aria-label={clip.title}
      className={`group relative flex min-w-0 overflow-hidden rounded-lg border bg-surface transition-colors ${list ? "flex-row items-center gap-3 p-2" : "flex-col"} ${p.selected ? "border-accent ring-1 ring-accent" : "border-border hover:border-border-hover"}`}
    >
      <div
        className={`relative shrink-0 overflow-hidden ${list ? "aspect-video w-28 rounded-md" : "aspect-[16/6.4] w-full"}`}
      >
        <ClipThumb
          scene={game.scene}
          seed={thumbSeed(clip.id)}
          className="absolute inset-0 size-full"
        />
        <button
          type="button"
          role="checkbox"
          aria-checked={p.selected}
          aria-label={`Select ${clip.title}`}
          onClick={p.onSelect}
          className="absolute top-2 left-2 inline-flex size-6 cursor-pointer items-center justify-center rounded-md bg-surface-raised text-text"
        >
          <Icon
            name={p.selected ? "checkboxChecked" : "checkbox"}
            className={`size-5 ${p.selected ? "text-accent" : ""}`}
          />
        </button>
        <span className="app-text-xs absolute right-2 bottom-2 rounded-md bg-surface-raised px-1.5 py-0.5 font-medium text-text tabular-nums">
          {formatDuration(clip.durationSec)}
        </span>
      </div>
      <div
        className={`flex min-w-0 flex-1 gap-2 ${list ? "flex-row items-center" : "flex-col p-3"}`}
      >
        <div className="relative min-w-0 flex-1">
          <p className="app-text-xs flex items-center gap-1.5 tracking-wide text-text-secondary uppercase">
            <Icon name="gamepad" className="size-3.5 shrink-0 text-accent" />
            <span className="truncate">{game.name}</span>
          </p>
          <h4
            data-clip-title
            className="app-text-base truncate font-semibold text-text"
          >
            {clip.title}
          </h4>
          <p className="app-text-sm text-text-secondary">
            {formatAgo(clip.minutesAgo)} • {clip.sizeMb} MB
          </p>
          {!list && (
            <button
              type="button"
              aria-label={`More actions for ${clip.title}`}
              onClick={p.onMore}
              className="absolute top-0 right-0 inline-flex size-7 cursor-pointer items-center justify-center rounded-md text-text-secondary hover:bg-surface-hover hover:text-text"
            >
              <Icon name="more" className="size-4" />
            </button>
          )}
        </div>
        <div className={`flex gap-1.5 ${list ? "w-72 shrink-0" : ""}`}>
          <Action
            label={`Play ${clip.title}`}
            icon="play"
            primary
            onClick={p.onPlay}
          />
          <Action
            label={`Edit ${clip.title}`}
            icon="editor"
            onClick={p.onEdit}
          />
          <Action
            label={`Favorite ${clip.title}`}
            icon="star"
            pressed={clip.favorite}
            onClick={p.onFavorite}
          />
          <Action
            label={`Delete ${clip.title}`}
            icon="trash"
            onClick={p.onDelete}
          />
          <Action
            label={`Open folder for ${clip.title}`}
            icon="folder"
            onClick={p.onFolder}
          />
        </div>
      </div>
    </article>
  );
}

export default ClipCard;
