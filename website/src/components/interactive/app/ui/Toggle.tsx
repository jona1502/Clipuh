/**
 * On/off switch (`role="switch"`, `aria-checked`). The label is the
 * accessible name; render visible status text next to it yourself.
 */
export function Toggle({
  label,
  checked,
  onChange,
  disabled,
  className,
}: {
  label: string;
  checked: boolean;
  onChange(v: boolean): void;
  disabled?: boolean;
  className?: string;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={`app-switch disabled:cursor-not-allowed disabled:opacity-50${className ? ` ${className}` : ""}`}
    />
  );
}

export default Toggle;
