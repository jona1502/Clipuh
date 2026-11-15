import { useRef } from "react";
import { CAPTURE_SOURCES, captureSourceById } from "../../sources";
import { dispatch, useApp } from "../../store";
import { Dropdown } from "../../ui/Dropdown";
import { Icon } from "../../ui/Icon";
import { PlannedBadge } from "../../ui/PlannedBadge";
import { Card } from "./Card";

const OPTIONS = CAPTURE_SOURCES.map((s) => ({
  value: s.id,
  label: s.name,
  detail: s.detail,
  icon: s.icon,
}));

export function SourceCard() {
  const sourceId = useApp((s) => s.capture.sourceId);
  const source = captureSourceById(sourceId);
  const ref = useRef<HTMLDivElement>(null);
  return (
    <Card
      icon="gamepad"
      title="Capture Source"
      subtitle="Select what to capture."
      data-capture-source-card=""
    >
      <div ref={ref} className="flex flex-1 flex-col gap-3">
        <Dropdown
          label="Source to capture"
          value={sourceId}
          options={OPTIONS}
          onChange={(v) =>
            dispatch({ type: "capture/set", patch: { sourceId: v } })
          }
          rootClassName="w-full"
          className="w-full border-accent bg-accent-subtle px-3 py-2"
          renderValue={() => (
            <>
              <span className="grid size-[2.4em] shrink-0 place-items-center rounded-lg bg-surface-raised">
                <Icon name={source.icon} className="size-[55%]" />
              </span>
              <span className="flex min-w-0 flex-col leading-tight">
                <span className="app-text-base truncate font-semibold">
                  {source.name}
                </span>
                <span className="app-text-sm truncate text-text-secondary">
                  {source.detail}
                </span>
              </span>
            </>
          )}
        />
        <span
          data-element="capture.changeSource"
          className="inline-flex items-center gap-2"
        >
          <button
            type="button"
            onClick={() =>
              ref.current
                ?.querySelector<HTMLButtonElement>("button[aria-haspopup]")
                ?.click()
            }
            className="app-text-sm inline-flex flex-1 cursor-pointer items-center justify-center gap-2 rounded-lg border border-border bg-surface-raised px-3 py-2 text-text transition-colors hover:bg-surface-hover"
          >
            <Icon name="crosshair" />
            Change Source
          </button>
          <PlannedBadge id="capture.changeSource" />
        </span>
      </div>
    </Card>
  );
}

export default SourceCard;
