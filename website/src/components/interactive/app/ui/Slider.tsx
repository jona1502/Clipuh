import type { CSSProperties } from "react";

/**
 * Native range input styled by `.demo-slider` (global.css). `format` sets
 * `aria-valuetext`; render a visible readout next to it if the design shows
 * one.
 */
export function Slider({
  label,
  min,
  max,
  step,
  value,
  onChange,
  format,
  disabled,
  className,
}: {
  label: string;
  min: number;
  max: number;
  step: number;
  value: number;
  onChange(v: number): void;
  format?(v: number): string;
  disabled?: boolean;
  className?: string;
}) {
  const fill = max > min ? ((value - min) / (max - min)) * 100 : 0;
  return (
    <input
      type="range"
      className={`demo-slider${className ? ` ${className}` : ""}`}
      aria-label={label}
      aria-valuetext={format ? format(value) : undefined}
      min={min}
      max={max}
      step={step}
      value={value}
      disabled={disabled}
      onChange={(e) => onChange(Number(e.currentTarget.value))}
      style={{ "--demo-fill": `${fill}%` } as CSSProperties}
    />
  );
}

export default Slider;
