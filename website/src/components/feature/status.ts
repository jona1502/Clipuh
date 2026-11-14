import type { FeatureStatus } from "@/data/features";

/**
 * Shared class source for the status pill, used by StatusBadge.astro and the
 * React demos so planned / in-development look identical everywhere.
 *
 * Contrast: text-text on accent-subtle (16 % accent over the surface) and
 * text-text-secondary on a transparent background both exceed 4.5:1.
 */
export function statusBadgeClass(
  status: FeatureStatus,
  size: "md" | "sm" = "md",
): string {
  const tone =
    status === "in-development"
      ? "bg-accent-subtle text-text border-transparent"
      : "bg-transparent text-text-secondary border-border-hover";
  const pad = size === "sm" ? "px-2 py-0 leading-5" : "px-2.5 py-0.5";
  return `inline-flex shrink-0 items-center gap-1.5 rounded-full border ${pad} text-caption font-semibold whitespace-nowrap ${tone}`;
}

export function statusDotClass(status: FeatureStatus): string {
  return status === "in-development"
    ? "size-1.5 rounded-full bg-accent-hover"
    : "size-1.5 rounded-full border border-text-muted";
}
