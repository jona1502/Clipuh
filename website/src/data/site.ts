/**
 * Site-wide content and metadata. Single source for name, copy, navigation
 * and download availability; components read from here instead of
 * hardcoding strings or URLs.
 */

/** Homepage section ids used as in-page navigation targets. */
export const sections = {
  hero: "top",
  features: "features",
  /** The interactive demos start with the instant replay section. */
  showcase: "instant-replay",
  download: "download",
} as const;

export interface NavLink {
  label: string;
  /** In-page anchor (`#id`) until the dedicated route exists. */
  href: string;
}

export const site = {
  name: "clipuh",
  tagline: "Your best moments. Always captured.",
  description:
    "clipuh is a native game clipping app for Windows. Save the moments that matter instantly, control every audio source, and turn clips into highlights without leaving your game.",

  /**
   * Primary navigation. Routes like /features do not exist yet, so links
   * point at homepage sections that are rendered on the page. Never add a
   * link whose target does not exist.
   */
  nav: [
    { label: "Features", href: `#${sections.features}` },
    { label: "Showcase", href: `#${sections.showcase}` },
    { label: "Download", href: `#${sections.download}` },
  ] satisfies NavLink[],

  /**
   * Download availability. No public release exists yet: `url` stays null
   * and the UI must render a "Coming soon" state. Set `url` to a real,
   * published release asset or releases page when one exists — never invent
   * one.
   */
  download: {
    platform: "Windows",
    available: false,
    url: null as string | null,
    statusLabel: "Coming soon",
    note: "clipuh is in active development. Windows builds will be published here.",
  },

  /**
   * Public source repository. The repository is private today, so this is
   * null and no GitHub link is rendered.
   */
  repositoryUrl: null as string | null,
} as const;
