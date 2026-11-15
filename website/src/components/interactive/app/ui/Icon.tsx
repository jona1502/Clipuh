import type { ReactNode } from "react";

/**
 * Inline SVG icon set covering every icon visible in the four app mockups
 * (capture, library, editor, settings). 24x24 grid, 1.75 stroke,
 * currentColor. Self-drawn; no icon font or dependency.
 */
const paths = {
  // ---- Navigation ----
  capture: (
    <>
      <path d="M4 8V6a2 2 0 0 1 2-2h2M16 4h2a2 2 0 0 1 2 2v2M20 16v2a2 2 0 0 1-2 2h-2M8 20H6a2 2 0 0 1-2-2v-2" />
      <circle cx="12" cy="12" r="3" />
    </>
  ),
  library: (
    <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z" />
  ),
  editor: (
    <>
      <circle cx="6" cy="6" r="2.5" />
      <circle cx="6" cy="18" r="2.5" />
      <path d="M8 7.5 20 18M8 16.5 20 6" />
    </>
  ),
  settings: (
    <>
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1Z" />
    </>
  ),
  // ---- Window chrome ----
  minimize: <path d="M5 12h14" />,
  maximize: <rect x="5" y="5" width="14" height="14" rx="1.5" />,
  close: <path d="M6 6l12 12M18 6 6 18" />,
  // ---- Chevrons / arrows ----
  chevronDown: <path d="m6 9 6 6 6-6" />,
  chevronUp: <path d="m6 15 6-6 6 6" />,
  chevronLeft: <path d="m15 6-6 6 6 6" />,
  chevronRight: <path d="m9 6 6 6-6 6" />,
  chevronUpDown: <path d="m8 9 4-4 4 4M8 15l4 4 4-4" />,
  arrowRight: <path d="M5 12h14M13 6l6 6-6 6" />,
  // ---- Sources / devices ----
  gamepad: (
    <>
      <path d="M7 7h10a5 5 0 0 1 4.9 6l-.6 3a2.6 2.6 0 0 1-4.6 1.1L15 15H9l-1.7 2.1a2.6 2.6 0 0 1-4.6-1.1l-.6-3A5 5 0 0 1 7 7Z" />
      <path d="M8 10v3M6.5 11.5h3" />
      <circle cx="15.5" cy="10.5" r=".6" fill="currentColor" />
      <circle cx="17.5" cy="12.5" r=".6" fill="currentColor" />
    </>
  ),
  monitor: (
    <>
      <rect x="3" y="4" width="18" height="12" rx="2" />
      <path d="M8 20h8M12 16v4" />
    </>
  ),
  window: (
    <>
      <rect x="3" y="4" width="18" height="16" rx="2" />
      <path d="M3 9h18M7 6.5h.01M10 6.5h.01" />
    </>
  ),
  monitorPlay: (
    <>
      <rect x="3" y="4" width="18" height="12" rx="2" />
      <path d="M8 20h8M12 16v4M10.5 8v4l3-2Z" />
    </>
  ),
  headphones: (
    <>
      <path d="M4 15v-3a8 8 0 0 1 16 0v3" />
      <rect x="3" y="14" width="4" height="6" rx="1.5" />
      <rect x="17" y="14" width="4" height="6" rx="1.5" />
    </>
  ),
  mic: (
    <>
      <rect x="9" y="3" width="6" height="11" rx="3" />
      <path d="M5 11a7 7 0 0 0 14 0M12 18v3M9 21h6" />
    </>
  ),
  volume: (
    <>
      <path d="M4 9h3l5-4v14l-5-4H4Z" />
      <path d="M16 9a4 4 0 0 1 0 6M18.5 6.5a7.5 7.5 0 0 1 0 11" />
    </>
  ),
  volumeMute: (
    <>
      <path d="M4 9h3l5-4v14l-5-4H4Z" />
      <path d="m16 9 5 6M21 9l-5 6" />
    </>
  ),
  crosshair: (
    <>
      <circle cx="12" cy="12" r="7" />
      <circle cx="12" cy="12" r="2" />
      <path d="M12 2v3M12 19v3M2 12h3M19 12h3" />
    </>
  ),
  database: (
    <>
      <ellipse cx="12" cy="6" rx="7" ry="3" />
      <path d="M5 6v12c0 1.7 3.1 3 7 3s7-1.3 7-3V6M5 12c0 1.7 3.1 3 7 3s7-1.3 7-3" />
    </>
  ),
  drive: (
    <>
      <path d="M3 13 5.5 6h13L21 13v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z" />
      <path d="M3 13h18M7 16h.01M10 16h.01" />
    </>
  ),
  video: (
    <>
      <rect x="3" y="6" width="13" height="12" rx="2" />
      <path d="m16 10 5-3v10l-5-3" />
    </>
  ),
  codec: (
    <>
      <rect x="3" y="6" width="13" height="12" rx="2" />
      <path d="m16 10 5-3v10l-5-3M7.5 10v4l3-2Z" />
    </>
  ),
  film: (
    <>
      <rect x="3" y="4" width="18" height="16" rx="2" />
      <path d="M7 4v16M17 4v16M3 9h4M3 15h4M17 9h4M17 15h4" />
    </>
  ),
  clapperboard: (
    <>
      <path d="M4 10h16v9a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1Z" />
      <path d="m4 10-.6-3.2a1 1 0 0 1 .8-1.2l13.7-2.4a1 1 0 0 1 1.2.8l.4 2.4L4 10ZM8.5 5l2.7 3.6M13.5 4.2l2.7 3.6" />
    </>
  ),
  clip: (
    <>
      <rect x="3" y="5" width="18" height="14" rx="3" />
      <path d="M10 9v6l5-3Z" />
    </>
  ),
  record: (
    <>
      <circle cx="12" cy="12" r="8" />
      <circle cx="12" cy="12" r="3" fill="currentColor" />
    </>
  ),
  keyboard: (
    <>
      <rect x="2.5" y="6" width="19" height="12" rx="2" />
      <path d="M6 10h.01M9 10h.01M12 10h.01M15 10h.01M18 10h.01M8 14h8" />
    </>
  ),
  replay: (
    <>
      <path d="M3.5 12a8.5 8.5 0 1 0 2.5-6" />
      <path d="M3 3.5V8h4.5" />
    </>
  ),
  palette: (
    <>
      <path d="M12 3a9 9 0 1 0 0 18c1.1 0 1.8-.9 1.5-1.9-.3-1 .4-2.1 1.5-2.1H17a4 4 0 0 0 4-4c0-5.5-4-10-9-10Z" />
      <circle cx="7.5" cy="11" r="1" fill="currentColor" />
      <circle cx="10" cy="7" r="1" fill="currentColor" />
      <circle cx="15" cy="7.5" r="1" fill="currentColor" />
    </>
  ),
  sun: (
    <>
      <circle cx="12" cy="12" r="4" />
      <path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" />
    </>
  ),
  moon: <path d="M20 14.5A8 8 0 1 1 9.5 4a6.5 6.5 0 0 0 10.5 10.5Z" />,
  info: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 11v5M12 8h.01" />
    </>
  ),
  // ---- Actions ----
  play: <path d="M7 4.5v15l12-7.5Z" fill="currentColor" />,
  pause: (
    <path d="M7 5h3v14H7zM14 5h3v14h-3z" fill="currentColor" stroke="none" />
  ),
  skipBack: <path d="M18 5v14L8 12ZM6 5v14" />,
  skipForward: <path d="M6 5v14l10-7ZM18 5v14" />,
  rewind10: (
    <>
      <path d="M4 12a8 8 0 1 0 2.3-5.7" />
      <path d="M4 3.5V7h3.5M9.5 10v5M12.5 10.5h2v4h-2Z" />
    </>
  ),
  forward10: (
    <>
      <path d="M20 12a8 8 0 1 1-2.3-5.7" />
      <path d="M20 3.5V7h-3.5M9.5 10v5M12.5 10.5h2v4h-2Z" />
    </>
  ),
  fullscreen: (
    <path d="M4 9V5a1 1 0 0 1 1-1h4M15 4h4a1 1 0 0 1 1 1v4M20 15v4a1 1 0 0 1-1 1h-4M9 20H5a1 1 0 0 1-1-1v-4" />
  ),
  more: (
    <>
      <circle cx="5.5" cy="12" r="1.3" fill="currentColor" stroke="none" />
      <circle cx="12" cy="12" r="1.3" fill="currentColor" stroke="none" />
      <circle cx="18.5" cy="12" r="1.3" fill="currentColor" stroke="none" />
    </>
  ),
  plus: <path d="M12 5v14M5 12h14" />,
  minus: <path d="M5 12h14" />,
  check: <path d="m5 12.5 4.5 4.5L19 7.5" />,
  checkCircle: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="m8 12.5 2.8 2.8L16 10" />
    </>
  ),
  star: (
    <path d="m12 3.5 2.6 5.3 5.9.9-4.3 4.1 1 5.8L12 16.9l-5.2 2.7 1-5.8-4.3-4.1 5.9-.9Z" />
  ),
  trash: <path d="M4 7h16M9 7V4.5h6V7M6.5 7l1 13h9l1-13M10 11v5M14 11v5" />,
  folder: (
    <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z" />
  ),
  folderOpen: (
    <path d="M3 18V6a1 1 0 0 1 1-1h5l2 2h7a1 1 0 0 1 1 1v2M3 18l2.5-7a1 1 0 0 1 1-.7H21l-3 8.1a1 1 0 0 1-1 .6H4a1 1 0 0 1-1-1Z" />
  ),
  search: (
    <>
      <circle cx="11" cy="11" r="6.5" />
      <path d="m20 20-4.4-4.4" />
    </>
  ),
  filter: <path d="M4 5h16l-6 7.5V19l-4-2v-4.5Z" />,
  grid: <path d="M4 4h7v7H4zM13 4h7v7h-7zM4 13h7v7H4zM13 13h7v7h-7z" />,
  list: <path d="M9 6h11M9 12h11M9 18h11M4.5 6h.01M4.5 12h.01M4.5 18h.01" />,
  pencil: <path d="M4 20h4L19 9a2.8 2.8 0 0 0-4-4L4 16Z M13.5 6.5l4 4" />,
  upload: (
    <path d="M12 15V4M7 9l5-5 5 5M4 15v4a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1v-4" />
  ),
  undo: <path d="M9 14 4 9l5-5M4 9h10a6 6 0 0 1 0 12h-3" />,
  redo: <path d="m15 14 5-5-5-5M20 9H10a6 6 0 0 0 0 12h3" />,
  select: <path d="M4 4h16v16H4z" strokeDasharray="3 2.5" />,
  razor: (
    <path d="M14.5 4.5a3.5 3.5 0 0 0-4.6 4.6L4 15v5h5l5.9-5.9a3.5 3.5 0 0 0 4.6-4.6l-2.3 2.3-2.4-.6-.6-2.4Z" />
  ),
  link: (
    <path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1" />
  ),
  magnet: (
    <path d="M5 4h4v8a3 3 0 0 0 6 0V4h4v8a7 7 0 0 1-14 0ZM5 8h4M15 8h4" />
  ),
  snowflake: (
    <path d="M12 2v20M3.3 7l17.4 10M3.3 17 20.7 7M9 4l3 2 3-2M9 20l3-2 3 2M4 10.5l3-.5-1-3M20 13.5l-3 .5 1 3M4 13.5l3 .5-1 3M20 10.5l-3-.5 1-3" />
  ),
  split: (
    <>
      <circle cx="6" cy="6" r="2.5" />
      <circle cx="6" cy="18" r="2.5" />
      <path d="M8 7.5 20 18M8 16.5 20 6" />
    </>
  ),
  lock: (
    <>
      <rect x="5" y="11" width="14" height="9" rx="2" />
      <path d="M8 11V8a4 4 0 0 1 8 0v3" />
    </>
  ),
  unlock: (
    <>
      <rect x="5" y="11" width="14" height="9" rx="2" />
      <path d="M8 11V8a4 4 0 0 1 7.5-2" />
    </>
  ),
  eye: (
    <>
      <path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12Z" />
      <circle cx="12" cy="12" r="3" />
    </>
  ),
  eyeOff: (
    <>
      <path d="M10 5.7A9.6 9.6 0 0 1 12 5.5c6 0 9.5 6.5 9.5 6.5a16 16 0 0 1-2.4 3.2M6.6 6.6A15.6 15.6 0 0 0 2.5 12S6 18.5 12 18.5a9 9 0 0 0 5.4-1.6" />
      <path d="M3 3l18 18M9.9 9.9a3 3 0 0 0 4.2 4.2" />
    </>
  ),
  checkbox: <rect x="4" y="4" width="16" height="16" rx="3" />,
  checkboxChecked: (
    <>
      <rect x="4" y="4" width="16" height="16" rx="3" />
      <path d="m8 12.5 2.8 2.8L16 9.5" />
    </>
  ),
} satisfies Record<string, ReactNode>;

export type IconName = keyof typeof paths;
export const ICON_NAMES = Object.keys(paths) as IconName[];

export function Icon({
  name,
  className,
}: {
  name: IconName;
  className?: string;
}) {
  return (
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.75}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
      className={className ?? "size-[1.15em] shrink-0"}
    >
      {paths[name]}
    </svg>
  );
}

export default Icon;
