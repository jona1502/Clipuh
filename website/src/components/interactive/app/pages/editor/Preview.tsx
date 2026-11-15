import type { CSSProperties } from "react";
import { ClipThumb, type ThumbScene } from "../../thumbs";
import { Icon } from "../../ui/Icon";
import { IconButton } from "../../ui/IconButton";
import { Slider } from "../../ui/Slider";
import { formatClock } from "./editorState";

export interface PreviewLook {
  boost: boolean;
  vignette: boolean;
  overlay: string;
}

interface Props {
  scene: ThumbScene;
  seed: number;
  playhead: number;
  duration: number;
  playing: boolean;
  outside: boolean;
  videoHidden: boolean;
  look: PreviewLook;
  volume: number;
  previewMuted: boolean;
  onTogglePlay(): void;
  onSeek(t: number): void;
  onSkip(delta: number): void;
  onToStart(): void;
  onToEnd(): void;
  onVolume(v: number): void;
  onToggleMute(): void;
  onFullscreen(): void;
}

/**
 * Preview of the open clip: the generated scene for its game (no footage),
 * plus the transport row (time, scrubber, skip, play, volume).
 */
export function Preview(p: Props) {
  const pan = (p.playhead / Math.max(1, p.duration)) * 6;
  const sceneStyle: CSSProperties = {
    transform: `scale(1.08) translateX(${3 - pan}%)`,
    filter: p.look.boost ? "saturate(1.6) contrast(1.08)" : undefined,
  };
  return (
    <section
      aria-label="Preview"
      className="@container flex min-h-0 min-w-0 flex-col overflow-hidden rounded-xl border border-border bg-surface"
    >
      <div
        data-editor-preview
        className="relative aspect-video min-h-0 overflow-hidden bg-background @min-[60rem]/app-window:aspect-auto @min-[60rem]/app-window:flex-1"
      >
        <div className="absolute inset-0" style={sceneStyle}>
          <ClipThumb scene={p.scene} seed={p.seed} className="h-full w-full" />
        </div>
        {p.look.vignette && (
          <div
            aria-hidden="true"
            className="pointer-events-none absolute inset-0"
            style={{
              background:
                "radial-gradient(ellipse at center, transparent 45%, var(--color-background) 100%)",
            }}
          />
        )}
        {(p.outside || p.videoHidden) && (
          <div
            aria-hidden="true"
            className="absolute inset-0 bg-background/70"
          />
        )}
        <span className="app-text-xs absolute top-2 left-2 rounded-sm bg-background/75 px-2 py-0.5 font-semibold text-text-secondary">
          Sample clip
        </span>
        {(p.outside || p.videoHidden) && (
          <span className="app-text-xs absolute top-2 right-2 rounded-sm bg-background/75 px-2 py-0.5 text-text-secondary">
            {p.videoHidden ? "Video track hidden" : "Outside trim"}
          </span>
        )}
        {p.look.overlay.trim() && (
          <span className="app-text-lg absolute inset-x-4 bottom-4 truncate text-center font-semibold text-text [text-shadow:0_1px_3px_var(--color-background)]">
            {p.look.overlay}
          </span>
        )}
      </div>

      <div
        className="flex flex-wrap items-center gap-x-2.5 gap-y-2 border-t border-border px-3 py-1.5 @[34rem]:flex-nowrap"
        data-element="editor.playback"
      >
        <p className="app-text-xs shrink-0 font-mono text-text tabular-nums">
          <span data-editor-time>
            {formatClock(p.playhead)}
            <span className="text-text-muted">
              {" "}
              / {formatClock(p.duration)}
            </span>
          </span>
        </p>
        <Slider
          label="Seek"
          min={0}
          max={p.duration}
          step={1}
          value={p.playhead}
          onChange={p.onSeek}
          format={(v) => `${formatClock(v)} of ${formatClock(p.duration)}`}
          className="min-w-20 flex-1 basis-24"
        />
        <div className="flex items-center gap-1">
          <IconButton
            label="Go to trim start"
            icon="skipBack"
            size="sm"
            onClick={p.onToStart}
          />
          <IconButton
            label="Back 10 seconds"
            icon="rewind10"
            size="sm"
            onClick={() => p.onSkip(-100)}
          />
          <button
            type="button"
            aria-label={p.playing ? "Pause" : "Play"}
            title={p.playing ? "Pause (Space)" : "Play (Space)"}
            onClick={p.onTogglePlay}
            className="grid size-9 cursor-pointer place-items-center rounded-full bg-accent text-accent-foreground hover:bg-accent-hover"
          >
            <Icon name={p.playing ? "pause" : "play"} className="size-4" />
          </button>
          <IconButton
            label="Forward 10 seconds"
            icon="forward10"
            size="sm"
            onClick={() => p.onSkip(100)}
          />
          <IconButton
            label="Go to trim end"
            icon="skipForward"
            size="sm"
            onClick={p.onToEnd}
          />
        </div>
        <div className="flex items-center gap-1.5">
          <IconButton
            label="Mute preview"
            icon={p.previewMuted ? "volumeMute" : "volume"}
            size="sm"
            pressed={p.previewMuted}
            onClick={p.onToggleMute}
          />
          <Slider
            label="Preview volume"
            min={0}
            max={100}
            step={1}
            value={p.previewMuted ? 0 : p.volume}
            onChange={p.onVolume}
            format={(v) => `${v}%`}
            className="hidden w-16 @[40rem]:block"
          />
          <IconButton
            label="Fullscreen"
            icon="fullscreen"
            size="sm"
            onClick={p.onFullscreen}
          />
        </div>
      </div>
    </section>
  );
}

export default Preview;
