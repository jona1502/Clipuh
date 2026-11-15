/**
 * Feature showcase content (W2). Single source for headings, copy and the
 * development status of every capability shown on the homepage.
 *
 * Status must stay truthful to the desktop app (crates/*):
 * - "in-development": implemented in the current development build.
 * - "planned": designed, not implemented yet. Demos may show it, but the UI
 *   must label it as planned.
 * Nothing is "released": no public build exists (see site.download).
 * Never add performance numbers here unless they are measured and sourced.
 */

export type FeatureStatus = "in-development" | "planned";

export const statusLabel: Record<FeatureStatus, string> = {
  "in-development": "In development",
  planned: "Planned",
};

export interface FeatureCopy {
  id: string;
  eyebrow: string;
  title: string;
  description: string;
  status: FeatureStatus;
  /** Short facts rendered as a list next to the demo. */
  points: string[];
}

/** Section ids. `features` is the first showcase section (header nav target). */
export const featureIds = {
  intro: "features",
  replay: "instant-replay",
  mixer: "audio-mixer",
  editor: "editor",
  performance: "performance",
  more: "more-features",
  cta: "download",
} as const;

export const intro = {
  eyebrow: "Features",
  title: "Everything around the clip, in one native app",
  description:
    "clipuh keeps a rolling replay of your game, records audio sources on their own tracks, and lets you trim the result before you share it.",
} as const;

export const replay: FeatureCopy = {
  id: featureIds.replay,
  eyebrow: "Instant replay",
  title: "Never miss the moment.",
  description:
    "Capture the moments that matter with a customizable hotkey and instant replay.",
  status: "in-development",
  points: [
    "Rolling replay of the last 15 seconds to 5 minutes",
    "Global hotkey saves the clip while you keep playing",
    "Recent footage stays in RAM, older footage spills to a temporary SSD cache",
  ],
};

export const mixer: FeatureCopy = {
  id: featureIds.mixer,
  eyebrow: "Audio",
  title: "Your audio. Your control.",
  description:
    "Choose which sources end up in your clip, set their levels, and keep them on separate tracks for later.",
  status: "in-development",
  points: [
    "Desktop audio and microphone with individual gain",
    "Optional separate tracks next to the mixed track",
    "Per-application sources like the game or Discord, each with mute and solo",
  ],
};

/**
 * Mixer demo sources. `status` controls the "Planned" tag in the demo:
 * the app has game, application (e.g. Discord), desktop and microphone
 * sources (AudioSourceType in clipper-storage, views/audio.rs).
 */
export const mixerSources: ReadonlyArray<{
  id: "game" | "discord" | "desktop" | "microphone";
  label: string;
  status: FeatureStatus;
}> = [
  { id: "game", label: "Game audio", status: "in-development" },
  { id: "discord", label: "Discord", status: "in-development" },
  { id: "desktop", label: "Desktop audio", status: "in-development" },
  { id: "microphone", label: "Microphone", status: "in-development" },
];

export const editor: FeatureCopy = {
  id: featureIds.editor,
  eyebrow: "Editor",
  title: "Make every clip yours.",
  description:
    "Trim a clip to the part that matters and export it as a new file. The original stays untouched.",
  status: "in-development",
  points: [
    "Set in and out points on a preview timeline",
    "Export writes a new MP4 with exact cuts and the enabled audio mix",
    "Multi-track audio timeline with per-track mute, solo, gain, split and undo",
  ],
};

export const performance: FeatureCopy = {
  id: featureIds.performance,
  eyebrow: "Architecture",
  title: "Built for your game, not against it.",
  description:
    "clipuh is designed to stay out of the way: native code, encoding on the GPU and bounded memory for the replay buffer. These are design goals, not benchmark claims.",
  status: "in-development",
  points: [
    "Native Rust application with a GPUI interface, no browser engine in the capture path",
    "H.264 encoded on the GPU through NVENC, AMF or Quick Sync, with no software fallback",
    "Replay buffer holds encoded frames only, within a configurable RAM budget",
    "Clips are saved by copying encoded data, not by re-encoding",
  ],
};

export interface BentoItem {
  title: string;
  description: string;
  status: FeatureStatus;
  /** Layout weight in the grid: "wide" spans two columns on desktop. */
  size: "wide" | "regular";
}

export const moreFeatures = {
  eyebrow: "And more",
  title: "Details that add up",
  items: [
    {
      title: "Customizable hotkeys",
      description: "Record your own shortcut for saving a replay.",
      status: "in-development",
      size: "regular",
    },
    {
      title: "Clip library",
      description: "Saved clips are listed in the app, ready to open or trim.",
      status: "in-development",
      size: "wide",
    },
    {
      title: "Resolution and FPS",
      description: "Pick output resolution, frame rate and quality preset.",
      status: "in-development",
      size: "wide",
    },
    {
      title: "Themes",
      description: "Dark, light or system appearance with accent presets.",
      status: "in-development",
      size: "regular",
    },
    {
      title: "Replay length",
      description: "Choose 15 s, 30 s, 1 min, 2 min or 5 min of history.",
      status: "in-development",
      size: "regular",
    },
    {
      title: "Source selection",
      description: "Capture a whole monitor or a single window.",
      status: "in-development",
      size: "regular",
    },
  ] satisfies BentoItem[],
} as const;

export const finalCta = {
  title: "clipuh is in active development",
  description:
    "There is no public build yet. Windows builds will be published here when they are ready.",
} as const;
