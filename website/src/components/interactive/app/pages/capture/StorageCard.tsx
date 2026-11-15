import { dispatch, estimateClipMb, useApp, type Codec } from "../../store";
import { Dropdown, type DropdownOption } from "../../ui/Dropdown";
import { Icon } from "../../ui/Icon";
import { PlannedBadge } from "../../ui/PlannedBadge";
import { Card } from "./Card";

const CODECS: DropdownOption<Codec>[] = [
  { value: "h264", label: "Codec: H.264 (High)" },
  { value: "hevc", label: "Codec: H.265 (HEVC)" },
  { value: "av1", label: "Codec: AV1" },
];
const CODEC_SHORT: Record<Codec, string> = {
  h264: "H.264 (High)",
  hevc: "H.265 (HEVC)",
  av1: "AV1",
};

export function StorageCard() {
  const capture = useApp((s) => s.capture);
  const bitrate = useApp((s) => s.settings.bitrateMbps);
  const mb = estimateClipMb(capture.replaySec, bitrate);
  return (
    <Card
      icon="database"
      title="Storage & Quality"
      subtitle="Estimated size and encoding settings."
    >
      <div
        data-element="capture.storageEstimate"
        className="rounded-lg bg-surface-raised p-3"
      >
        <p className="app-text-xs flex items-center gap-2 text-text-muted">
          Estimate
          <PlannedBadge id="capture.storageEstimate" />
        </p>
        <p data-estimate="" className="app-text-xl">
          ≈ <strong className="app-text-2xl font-bold">{mb} MB</strong> / Clip
        </p>
        <p className="app-text-sm text-text-secondary">
          {capture.resolution} · {capture.fps} FPS ·{" "}
          {CODEC_SHORT[capture.codec]}
        </p>
      </div>
      <Dropdown
        label="Codec"
        value={capture.codec}
        options={CODECS}
        onChange={(v) => dispatch({ type: "capture/set", patch: { codec: v } })}
        plannedId="capture.codec"
        rootClassName="w-full"
        className="w-full py-2"
        renderValue={(o) => (
          <>
            <Icon name="video" />
            <span className="truncate">{o?.label}</span>
          </>
        )}
      />
    </Card>
  );
}

export default StorageCard;
