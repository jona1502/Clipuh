import { useEffect, useRef, useState } from "react";
import { gameById } from "../../mockData";
import {
  dispatch,
  useApp,
  visibleClips,
  type Clip,
  type PageId,
} from "../../store";
import { ClipThumb } from "../../thumbs";
import { Icon } from "../../ui/Icon";
import { IconButton } from "../../ui/IconButton";
import { PlannedBadge } from "../../ui/PlannedBadge";
import { Card } from "./Card";

const fmt = (sec: number) =>
  `${Math.floor(sec / 60)}:${String(sec % 60).padStart(2, "0")}`;

function RowMenu({ clip, onEdit }: { clip: Clip; onEdit(): void }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    if (!open) return;
    const down = (e: PointerEvent) => {
      if (!ref.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", down);
    return () => document.removeEventListener("pointerdown", down);
  }, [open]);
  const item =
    "app-text-sm block w-full cursor-pointer rounded-md px-2.5 py-1.5 text-left whitespace-nowrap text-text-secondary hover:bg-surface-hover hover:text-text";
  return (
    <span
      ref={ref}
      className="relative"
      onKeyDown={(e) => {
        if (e.key === "Escape" && open) {
          e.stopPropagation();
          setOpen(false);
        }
      }}
    >
      <IconButton
        label={`More actions for ${clip.title}`}
        icon="more"
        size="sm"
        className="border-transparent bg-transparent"
        onClick={() => setOpen((o) => !o)}
      />
      {open && (
        <div
          role="menu"
          aria-label={`Actions for ${clip.title}`}
          className="absolute top-full right-0 z-30 mt-1 min-w-40 rounded-lg border border-border-hover bg-surface-raised p-1 shadow-lg"
        >
          <button
            type="button"
            role="menuitem"
            className={item}
            onClick={() => {
              dispatch({ type: "clip/toggleFavorite", id: clip.id });
              setOpen(false);
            }}
          >
            {clip.favorite ? "Unfavorite" : "Favorite"}
          </button>
          <button
            type="button"
            role="menuitem"
            className={item}
            onClick={() => {
              setOpen(false);
              onEdit();
            }}
          >
            Open in editor
          </button>
        </div>
      )}
    </span>
  );
}

export function RecentCard({ navigate }: { navigate(p: PageId): void }) {
  const clips = useApp((s) => visibleClips(s)).slice(0, 3);
  const edit = (id: string) => {
    dispatch({ type: "editor/open", id });
    navigate("editor");
  };
  return (
    <Card
      icon="monitorPlay"
      title="Recent Clips"
      subtitle="Your latest saved moments."
      data-element="capture.recentClips"
      action={<PlannedBadge id="capture.recentClips" />}
    >
      <ul className="flex flex-col gap-1">
        {clips.map((clip) => {
          const game = gameById(clip.gameId);
          return (
            <li
              key={clip.id}
              data-recent-clip={clip.id}
              className="flex items-center gap-3 rounded-lg p-1.5 hover:bg-surface-hover"
            >
              <span className="relative aspect-video w-[22%] max-w-24 shrink-0 overflow-hidden rounded-md">
                <ClipThumb
                  scene={game.scene}
                  seed={Number(clip.id.replace(/\D/g, ""))}
                  className="size-full"
                />
                <span className="app-text-xs absolute right-1 bottom-1 rounded bg-surface/80 px-1 text-text">
                  {fmt(clip.durationSec)}
                </span>
              </span>
              <span className="flex min-w-0 flex-1 flex-col leading-tight">
                <span className="app-text-sm flex items-center gap-1.5 font-semibold">
                  <span className="truncate">{clip.title}</span>
                  {clip.favorite && (
                    <span
                      data-favorite-star=""
                      role="img"
                      aria-label="Favorite"
                    >
                      <Icon
                        name="star"
                        className="size-[1em] fill-current text-warning"
                      />
                    </span>
                  )}
                </span>
                <span className="app-text-xs truncate text-text-secondary">
                  {game.name}
                </span>
              </span>
              <IconButton
                label={`Play ${clip.title}`}
                icon="play"
                size="sm"
                onClick={() => edit(clip.id)}
              />
              <RowMenu clip={clip} onEdit={() => edit(clip.id)} />
            </li>
          );
        })}
      </ul>
      <button
        type="button"
        onClick={() => navigate("library")}
        className="app-text-sm inline-flex cursor-pointer items-center justify-center gap-2 rounded-lg border border-border bg-surface-raised px-3 py-2 text-text transition-colors hover:bg-surface-hover"
      >
        View All Clips
        <Icon name="arrowRight" />
      </button>
    </Card>
  );
}

export default RecentCard;
