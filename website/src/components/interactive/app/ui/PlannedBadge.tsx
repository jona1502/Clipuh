import { statusBadgeClass, statusDotClass } from "@/components/feature/status";
import { isPlanned, type ElementId } from "@/data/appStatus";
import { statusLabel } from "@/data/features";

/**
 * "Planned" pill for an app element that is not implemented in the desktop
 * app (status from src/data/appStatus.ts). Renders nothing otherwise.
 */
export function PlannedBadge({
  id,
  className,
}: {
  id: ElementId;
  className?: string;
}) {
  if (!isPlanned(id)) return null;
  return (
    <span
      data-planned-badge={id}
      className={`${statusBadgeClass("planned", "sm")}${className ? ` ${className}` : ""}`}
    >
      <span className={statusDotClass("planned")} aria-hidden="true" />
      {statusLabel.planned}
    </span>
  );
}

export default PlannedBadge;
