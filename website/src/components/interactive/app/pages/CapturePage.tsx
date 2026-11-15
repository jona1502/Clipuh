import type { PageProps } from "../types";
import { AudioMixer } from "./capture/AudioMixer";
import { RecentCard } from "./capture/RecentCard";
import { SourceCard } from "./capture/SourceCard";
import { StorageCard } from "./capture/StorageCard";

/** Capture page: source, storage estimate, recent clips and audio mixer. */
export default function CapturePage({ navigate }: PageProps) {
  return (
    <div className="flex flex-col gap-3">
      <div className="grid gap-3 @min-[48rem]/app-window:grid-cols-2 @min-[60rem]/app-window:grid-cols-[1fr_1.15fr_1fr]">
        <SourceCard />
        <StorageCard />
        <div className="min-w-0 @min-[48rem]/app-window:col-span-2 @min-[60rem]/app-window:col-span-1">
          <RecentCard navigate={navigate} />
        </div>
      </div>
      <AudioMixer />
    </div>
  );
}
