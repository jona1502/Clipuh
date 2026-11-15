# clipuh website

Official website for clipuh. Static site built with Astro; fully isolated from
the Rust workspace (no Cargo involvement in either direction).

## Stack

- Astro 7 (static output, no adapter, no backend)
- TypeScript (strict)
- Tailwind CSS 4 via `@tailwindcss/vite`
- React 19 for interactive islands only

## Commands

Run from `/website` (Node >= 22.23.4, npm):

| Command                | Purpose                                             |
| ---------------------- | --------------------------------------------------- |
| `npm install`          | Install dependencies                                |
| `npm run dev`          | Dev server at http://localhost:4321                 |
| `npm run build`        | Static production build into `dist/`                |
| `npm run preview`      | Serve the production build locally                  |
| `npm run test:unit`    | Vitest unit tests (store, editor state, app status) |
| `npm run test:browser` | Playwright browser specs (needs a build first)      |
| `npm run check`        | Astro/TypeScript diagnostics + formatting check     |
| `npm run format`       | Format with Prettier                                |

## Structure

```text
website/
├── public/                 static files served as-is (favicons, clipuh logo)
├── src/
│   ├── components/
│   │   ├── ui/             Button, Container
│   │   ├── feature/        DemoFrame, BentoCard, StatusBadge, status.ts
│   │   ├── layout/         Header (chapter nav), Footer, Logo
│   │   ├── sections/       Hero
│   │   ├── showcase/       TourChapter, PerformanceSection, MoreFeatures, FinalCta
│   │   └── interactive/    React islands (.tsx): app/ (the app tour), editor/, shared/
│   ├── data/               site constants, content, appStatus.ts (element status)
│   ├── layouts/            page shells (BaseLayout.astro)
│   ├── pages/              file-based routes
│   └── styles/
│       ├── global.css      Tailwind entry, base styles, .reveal, .glass-panel
│       ├── app-window.css  per-window theme/accent tokens and app layout
│       └── tokens.css      semantic design tokens
├── astro.config.mjs
└── tsconfig.json
```

Directories are created when their first file exists. Planned as the site grows:

- `src/lib/`, `src/types/` — helpers and shared types
- `src/content/` — MDX docs and changelog (content collections)
- `public/images/{app,features}/`, `public/fonts/`

Planned routes: `/features`, `/showcase`, `/download`, `/changelog`, `/docs`, `/demo`.

## Conventions

- **Astro first.** Static content is an `.astro` component and ships no JavaScript.
- **React only for interactivity.** Islands live in `src/components/interactive/`
  and are mounted with a `client:*` directive (prefer `client:visible` /
  `client:idle`). A page without islands loads no React.
- **Tokens, not raw colors.** Use the semantic utilities from `tokens.css`
  (`bg-surface`, `text-text-secondary`, `border-border`, `text-accent`).
  Values mirror `crates/clipper-ui/src/theme/palette.rs`; keep them in sync.
- **Imports** use the `@/` alias for `src/`.
- `src/data/site.ts` is the single source for name, tagline and SEO defaults.
- Set `site` in `astro.config.mjs` once the production domain is known
  (enables canonical URLs and later a sitemap).

## Homepage: the app tour

`src/pages/index.astro`: Hero → four chapters (`#capture`, `#library`,
`#editor`, `#settings`) → Performance → More features → Final CTA. The header
nav links to the four chapters (mobile menu below 768px).

- **Islands.** Every window is one `AppTour` island
  (`interactive/app/AppTour.tsx`): the hero mounts it with `client:idle` on the
  Library page, each `TourChapter` with `client:visible` on its own page. The
  islands share one module-level store (`app/store.ts`, `useSyncExternalStore`),
  so favorites, renames, deletes, accent, theme and top-bar values sync across
  windows. Each window keeps its own current page. Page components are
  `React.lazy` chunks loaded when a window shows them.
- **Hero choice.** The server render of the hero window is visible at once and
  the LCP element stays text (Lighthouse 12: mobile LCP 2.5 s, desktop 0.6 s,
  both on the hero headline block, CLS 0), so the hero keeps the live island
  instead of a static image (a server-only render would leave the lazy page as
  a skeleton). The hero window is a preview: it sits in an `inert` wrapper (no
  focus, no clicks; the demo label stays outside it), so keyboard users go from
  the hero CTAs straight to the Capture chapter. Below 768px it shows the
  window's top part with `overflow: clip` and a fade.
- **JS budget.** At most 150 KB gzip loaded when the first window hydrates.
  Measured: 83.9 KB at hero hydration (react-dom client 65.7 KB, Dropdown
  7.3 KB, AppTour 3.9 KB, LibraryPage 3.9 KB, react 3.0 KB, the rest under
  2 KB each), 87.8 KB once the Capture chapter is visible. Re-measure after
  changes: collect the `.js` responses until the hero island hydrates and gzip
  (`-9`) the matching files in `dist/`.
- **Visual direction.** Raycast-like pacing, own assets only: near-black
  `canvas`, gradient chapter headlines (`text-chapter text-gradient`), each
  window on a `.glass-panel` backdrop with an accent glow, three short feature
  blocks per chapter, glass cards for Performance and More features, and a
  self-made SVG sphere-and-ring behind the final CTA. Motion is limited to
  `.reveal`, the hero fade-in and a slow CTA float, all off under reduced
  motion.

## Design system

Site chrome is dark only: one theme in `src/styles/tokens.css`, a Tailwind v4
`@theme` block, so every token is also a utility. App windows theme themselves:
each `[data-app-window]` re-declares the same tokens per light/dark theme and
accent in `src/styles/app-window.css` (driven by the shared Settings store), so
a window can be light or recolored without touching the site. The accent
swatch variables `--app-accent-*` also live on `:root` for site swatches.
Never use raw hex in components. The one exception is the generated clip
thumbnail illustrations in `interactive/app/thumbs.tsx`, which keep their own
fixed scene palette (they are artwork, not UI).

### Color tokens

| Token (utility suffix)                       | Purpose                                           |
| -------------------------------------------- | ------------------------------------------------- |
| `background`, `background-elevated`          | Page canvas; header/footer bands (app sidebar)    |
| `surface`, `surface-raised`, `surface-hover` | Cards (primary), raised/inputs (secondary), hover |
| `border`, `border-hover`                     | Default and emphasized borders                    |
| `text`, `text-secondary`, `text-muted`       | Text hierarchy (`muted` for captions/meta)        |
| `accent`, `accent-hover`, `accent-subtle`    | Brand color, hover fill, tinted backgrounds       |
| `accent-foreground`                          | Text on accent fills                              |
| `focus-ring`                                 | Focus outline (set globally on `:focus-visible`)  |
| `success`, `warning`, `error`                | Status                                            |
| `canvas`                                     | Web-only near-black page background               |
| `glass`, `glass-tint`, `glass-border`        | Glass panels (`glass-highlight` = top edge)       |
| `glow`, `glow-soft`                          | Accent light behind windows and the CTA shape     |

Examples: `bg-surface`, `text-text-secondary`, `border-border-hover`, `bg-accent-subtle`.

### Changing the accent

Edit `--color-accent` in `tokens.css`. Hover (mix 12% white), subtle (16% alpha)
and focus ring (70% alpha) are derived with `color-mix`. The one manual step is
`--color-accent-foreground`: pick white or `#111118`, whichever has the higher
WCAG contrast against the new accent (same rule as `foreground_for` in
`palette.rs`). Current `#9B5CF6`: white 4.00:1, ink 4.70:1, so ink.

### Typography scale

Fluid tokens; each utility sets size, line height, letter spacing and weight.
`text-display` (hero), `text-h1`, `text-h2`, `text-h3`, `text-lead` (intro
paragraph), `text-body`, `text-small`, `text-caption`. Bare `h1`-`h3` get the
matching style by default in `global.css`.

### Layout, radius, shadow, breakpoints

- Widths: `max-w-page` (1200px), `max-w-narrow` (736px). Vertical rhythm:
  `py-section` (4rem to 7rem fluid). Sticky header height token `--spacing-header`
  (72px) drives `scroll-padding-top`.
- Radius: `rounded-sm/md/lg/xl/2xl/3xl` (6/8/12/16/20/28px). Shadows:
  `shadow-sm/md/lg` (subtle), `shadow-glass` (inner top highlight),
  `shadow-float` (floating windows). Blur: `backdrop-blur-glass` (20px).
- Type: `text-chapter` for chapter headlines, `.text-gradient` (global.css)
  for the white-to-secondary headline gradient, `.glass-panel` for glass
  surfaces.
- Breakpoints are Tailwind defaults (`md` 768, `lg` 1024, `xl` 1280). Design and
  QA at 375, 430, 768, 1024, 1280, 1440, 1920. Mobile first.

### Font

Inter Variable via `@fontsource-variable/inter`, self-hosted (no CDN), SIL OFL
1.1 (free to redistribute). Chosen for neutral, highly legible UI text, full
weight axis in one file, and a well-known metric-matched fallback. Only the
latin subset woff2 is bundled (`font-display: swap`), preloaded in
`BaseLayout.astro`. `Inter Fallback` (Arial, size-adjusted) limits layout shift.

### UI components (`src/components/ui/`, zero client JS)

All accept `class` and pass remaining attributes to the root element.

```astro
<Button href="#download" variant="primary" size="lg">
  <svg slot="icon" ... />
  Download
</Button>
<Button variant="secondary">Action</Button>
<Button href="/x" disabled>
  Soon
</Button>{" "}
<!-- aria-disabled span, not clickable -->
<Container size="narrow" as="section" id="faq">
  ...
</Container>
```

- `Button`: `href?`, `variant?: "primary" | "secondary" | "ghost"` (primary),
  `size?: "md" | "lg"` (md, 40px; lg 44px), `disabled?`, `type?` (button);
  slots: default, `icon`.
- `Container`: `size?: "page" | "narrow"`, `as?: HTMLTag` (div). Padding 16/24/32px.

## Feature components

Zero-JS Astro primitives in `src/components/feature/`; copy comes from
`src/data/features.ts` (`tourChapters`, `performance`, `moreFeatures`).

```astro
<DemoFrame label="Interactive demo · sample data" minHeight="32rem">
  <AppTour client:visible initialPage="library" />
</DemoFrame>
<BentoCard title="..." description="..." status="planned" size="wide" />
```

- `StatusBadge`: `status` ("in-development" | "planned"), `class?`. Classes come
  from `feature/status.ts` (`statusBadgeClass`), shared with the React islands.
  Status rules live in `src/data/features.ts`: "planned" = not implemented in
  the app; nothing is "released" (no public build).
- `DemoFrame`: `label?` (the tour uses "Interactive demo · sample data"),
  `title?`, `minHeight?` (default `20rem`) and `minHeightLg?` (from `lg`), both
  prevent layout shift, default slot.
- `BentoCard`: `title`, `description`, `status`, `size?: "wide" | "regular"`;
  non-interactive glass `<article>`, default slot for extra content.
- App elements: every control in an app window that the desktop app lacks
  carries `<PlannedBadge id="..." />`; its status comes from
  `src/data/appStatus.ts` (with the `crates/` evidence next to each id).
- Islands use `useReducedMotion()` from `interactive/shared/useReducedMotion.ts`
  (no autoplay or looping meters when true).
- Shared classes in `global.css`: `.demo-slider` (native range, optional inline
  `--demo-fill`), `.reveal` (scroll reveal, CSS only via
  `animation-timeline: view()`; no-op without support or under reduced motion,
  so content is never hidden), `.glass-panel`, `.text-gradient`.

Honesty rules: every app window is visibly labelled "Interactive demo · sample
data" and is not a real capture; every chapter and feature shows its status
badge; every app element not implemented in the desktop app shows "Planned"
(from `appStatus.ts`) and is never presented as available; no performance
numbers unless measured and sourced; no real game names, logos or footage
(invented games and generated thumbnails only) and no hardware or vendor brand
names ("GPU encoder", "USB microphone").

## Browser regression checks

```sh
npm ci
npx playwright install chromium
npm run build
npm run test:browser
```

Playwright starts a local preview on port 4392. Stop any other Astro preview
for this checkout first (`npx astro preview stop`). Unit tests run with
`npm run test:unit` (vitest; on Windows run it from a cwd with an uppercase
drive letter, `D:\...`). Browser specs:

- `tour-page.spec.ts`: all seven QA widths (375, 430, 768, 1024, 1280, 1440,
  1920): no console errors or 4xx, no horizontal page scroll, header nav (mobile
  menu below 768px) reaches each chapter, chapter headings visible, stacked
  window layout and in-window timeline scroll at 375px; no-JavaScript fallback.
- `tour-shell.spec.ts`: per-window navigation, shared top bar, dropdowns.
- `tour-capture`, `tour-library`, `tour-editor`, `tour-settings`: each page's
  interactions, Planned badges and reduced motion.
- `tour-integration.spec.ts`: state shared across windows (favorite, delete,
  Open in Editor, rename).

Screenshots and failed traces are written to the ignored `test-results/`
directory.

Hotkey recording in the Settings window is scoped to that window: it does not
configure a global shortcut or persist settings, and Tab / Shift+Tab always
leave the row.
