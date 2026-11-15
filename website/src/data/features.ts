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
  /** Short facts rendered as cards next to the diagram. */
  points: string[];
}

/** Section ids of the non-chapter homepage sections. */
export const featureIds = {
  performance: "performance",
  more: "more-features",
  cta: "download",
} as const;

export const performance: FeatureCopy = {
  id: featureIds.performance,
  eyebrow: "Architecture",
  title: "Built for your game, not against it.",
  description:
    "clipuh is designed to stay out of the way: native code, encoding on the GPU and bounded memory for the replay buffer. These are design goals, not benchmark claims.",
  status: "in-development",
  points: [
    "Native Rust application, no browser engine in the capture path",
    "H.264 encoded on the GPU's hardware encoder, with no software fallback",
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

/** App tour chapters: one homepage section per app page. */
export type TourChapterId = "capture" | "library" | "editor" | "settings";

export interface ChapterCopy {
  /** Section id; equals the app page shown in the chapter window. */
  id: TourChapterId;
  eyebrow: string;
  title: string;
  description: string;
  status: FeatureStatus;
  /** Short feature blocks under the window. */
  points: { title: string; body: string }[];
}

export const tourChapters: Record<TourChapterId, ChapterCopy> = {
  capture: {
    id: "capture",
    eyebrow: "Capture",
    title: "Never miss the moment.",
    description:
      "Pick a source, set the quality once, and let the replay buffer run in the background. One hotkey saves what just happened.",
    status: "in-development",
    points: [
      {
        title: "Instant replay",
        body: "Keep the last 15 seconds to 5 minutes, ready to save.",
      },
      {
        title: "Size estimate",
        body: "See roughly how large a clip gets before you record it.",
      },
      {
        title: "Audio mixer",
        body: "Game, desktop and microphone, each with its own level.",
      },
    ],
  },
  library: {
    id: "library",
    eyebrow: "Library",
    title: "Every clip, right where you left it.",
    description:
      "Saved clips land in one library. Search, sort and favorite them, or select a few and clean up in one go.",
    status: "in-development",
    points: [
      {
        title: "Tabs and search",
        body: "Favorites, game clips and recordings, filtered as you type.",
      },
      {
        title: "Bulk actions",
        body: "Select several clips to delete, favorite or open them.",
      },
      {
        title: "Grid or list",
        body: "Browse by thumbnail or scan a compact list.",
      },
    ],
  },
  editor: {
    id: "editor",
    eyebrow: "Editor",
    title: "Cut straight to the play.",
    description:
      "Trim a clip to the part that matters and export it as a new file. The original stays untouched.",
    status: "in-development",
    points: [
      {
        title: "Precise trim",
        body: "Drag the handles or type exact start and end times.",
      },
      {
        title: "Separate audio tracks",
        body: "Game, desktop and microphone on their own lanes.",
      },
      {
        title: "Export",
        body: "Writes a new MP4 with the cut and the enabled audio.",
      },
    ],
  },
  settings: {
    id: "settings",
    eyebrow: "Settings",
    title: "Set it up once. Then just play.",
    description:
      "Video, audio, replay, hotkeys, storage and appearance live on one page. Theme and accent apply as you pick them.",
    status: "in-development",
    points: [
      {
        title: "Quality presets",
        body: "Resolution, frame rate, encoder and bitrate.",
      },
      {
        title: "Your hotkeys",
        body: "Record your own shortcut for saving a clip.",
      },
      {
        title: "Your look",
        body: "Dark, light or system theme with accent colors.",
      },
    ],
  },
};
