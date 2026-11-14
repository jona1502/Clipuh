import { memo } from "react";

interface Props {
  path: string;
  bars: number;
  muted: boolean;
  planned: boolean;
}

/** Static waveform. Memoized so playhead ticks do not re-render it. */
export const Waveform = memo(function Waveform({
  path,
  bars,
  muted,
  planned,
}: Props) {
  return (
    <svg
      viewBox={`0 0 ${bars} 100`}
      preserveAspectRatio="none"
      aria-hidden="true"
      className={[
        "absolute inset-0 h-full w-full transition-opacity duration-150",
        planned ? "text-text-muted" : "text-accent",
        muted ? "opacity-20" : "opacity-90",
      ].join(" ")}
    >
      <path d={path} fill="currentColor" />
    </svg>
  );
});
