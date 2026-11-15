import { useEffect, useState } from "react";
import { useReducedMotion } from "@/components/interactive/shared/useReducedMotion";

const SEGMENTS = 32;

/**
 * Segmented level meter (green, then yellow, then red). `level` is 0..1.
 * With `animated` the level flickers around `level` like live audio; under
 * prefers-reduced-motion it stays static.
 */
export function Meter({
  level,
  animated,
  label = "Level",
  className,
}: {
  level: number;
  animated: boolean;
  label?: string;
  className?: string;
}) {
  const reduced = useReducedMotion();
  const live = animated && !reduced && level > 0;
  const [jitter, setJitter] = useState(0);
  useEffect(() => {
    if (!live) {
      setJitter(0);
      return;
    }
    const t = window.setInterval(
      () => setJitter((Math.random() - 0.6) * 0.18),
      110,
    );
    return () => window.clearInterval(t);
  }, [live]);
  const shown = Math.min(1, Math.max(0, level + (live ? jitter : 0)));
  const on = Math.round(shown * SEGMENTS);
  return (
    <div
      role="meter"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(level * 100)}
      className={`app-meter${className ? ` ${className}` : ""}`}
    >
      {Array.from({ length: SEGMENTS }, (_, i) => (
        <span
          key={i}
          data-on={i < on ? "" : undefined}
          data-zone={
            i < SEGMENTS * 0.6 ? "low" : i < SEGMENTS * 0.85 ? "mid" : "high"
          }
        />
      ))}
    </div>
  );
}

export default Meter;
