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

| Command           | Purpose                                         |
| ----------------- | ----------------------------------------------- |
| `npm install`     | Install dependencies                            |
| `npm run dev`     | Dev server at http://localhost:4321             |
| `npm run build`   | Static production build into `dist/`            |
| `npm run preview` | Serve the production build locally              |
| `npm run check`   | Astro/TypeScript diagnostics + formatting check |
| `npm run format`  | Format with Prettier                            |

## Structure

```text
website/
├── public/                 static files served as-is
│   └── icons/
├── src/
│   ├── components/
│   │   ├── ui/             Button, Container, SectionHeading
│   │   ├── feature/        FeatureSection, DemoFrame, BentoCard, StatusBadge, status.ts
│   │   ├── showcase/       homepage showcase sections (Replay, Mixer, Editor, ...)
│   │   └── interactive/    React islands (.tsx)
│   ├── data/               site constants and content data
│   ├── layouts/            page shells (BaseLayout.astro)
│   ├── pages/              file-based routes
│   └── styles/
│       ├── global.css      Tailwind entry + base element styles
│       └── tokens.css      semantic design tokens
├── astro.config.mjs
└── tsconfig.json
```

Directories are created when their first file exists. Planned as the site grows:

- `src/components/layout/` — header, footer, navigation (`.astro`)
- `src/components/sections/` — page sections (`.astro`)
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

Islands live in `src/components/interactive/`: `ReplayDemo`, `MixerDemo`,
`EditorDemo` (with `editor/`), mounted with `client:visible`. The React runtime
loads when the first demo scrolls into view (about 66 KB gzip); the rest of the
page ships no JavaScript.

## Design system

Single theme (dark only) in `src/styles/tokens.css`, a Tailwind v4 `@theme`
block, so every token is also a utility. Never use raw hex in components.

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
- Radius: `rounded-sm/md/lg/xl` (6/8/12/16px). Shadows: `shadow-sm/md/lg`, subtle.
- Breakpoints are Tailwind defaults (`md` 768, `lg` 1024, `xl` 1280). Design and
  QA at 375, 430, 768, 1280, 1440, 1920. Mobile first.

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
<SectionHeading
  eyebrow="Features"
  title="Capture everything"
  description="..."
  level={2}
  align="center"
/>
```

- `Button`: `href?`, `variant?: "primary" | "secondary" | "ghost"` (primary),
  `size?: "md" | "lg"` (md, 40px; lg 44px), `disabled?`, `type?` (button);
  slots: default, `icon`.
- `Container`: `size?: "page" | "narrow"`, `as?: HTMLTag` (div). Padding 16/24/32px.
- `SectionHeading`: `title`, `eyebrow?`, `description?`, `level?: 1 | 2 | 3` (2),
  `align?: "left" | "center"` (left).

## W2 feature components

Zero-JS Astro primitives in `src/components/feature/`; copy comes from
`src/data/features.ts` (`FeatureCopy`).

```astro
<FeatureSection id={replay.id} copy={replay} reverse>
  <DemoFrame slot="demo" title="Replay buffer" minHeight="22rem">
    <ReplayDemo client:visible />
  </DemoFrame>
</FeatureSection>
<BentoCard title="..." description="..." status="planned" size="wide" />
```

- `FeatureSection`: `id`, `copy`, `reverse?`, `class?`; named slot `demo`.
  Text first on mobile; demo beside text from `lg`. Top divider, `py-section`.
- `StatusBadge`: `status` ("in-development" | "planned"), `class?`. Classes come
  from `feature/status.ts` (`statusBadgeClass`), shared with the React demos.
  Status rules live in `src/data/features.ts`: "planned" = not implemented in
  the app; nothing is "released" (no public build).
- `DemoFrame`: `label?` (default "Interactive demo"; used as "Interactive demo ·
  nothing is recorded", "... · no audio is captured", "... · sample timeline",
  "Architecture overview"), `title?`, `minHeight?` (default `20rem`) and
  `minHeightLg?` (from `lg`), both prevent layout shift, default slot.
- `BentoCard`: `title`, `description`, `status`, `size?: "wide" | "regular"`;
  non-interactive `<article>`, default slot for extra content.
- Islands: mount with `client:visible`; use `useReducedMotion()` from
  `interactive/shared/useReducedMotion.ts` (no autoplay or looping when true).
- Shared classes in `global.css` ("W2 demo + reveal" block): `.demo-toggle`
  (use `aria-pressed`), `.demo-slider` (native range, optional inline
  `--demo-fill`), `.reveal` (scroll reveal, CSS only via
  `animation-timeline: view()`; no-op without support or under reduced motion,
  so content is never hidden; no script needed).

Honesty rules: every demo is visibly labelled (`DemoFrame` label) and is not a
real capture; every feature shows its status badge; `planned` features are never
presented as available; no performance numbers unless measured and sourced.

## Browser regression checks

```sh
npm ci
npx playwright install chromium
npm run build
npm run test:browser
```

Playwright starts a local preview on port 4392. Stop any other Astro preview
for this checkout first (`npx astro preview stop`). Tests cover all seven W2
viewport widths, mobile navigation, hotkey selection and activation, replay
history, mixer gain and disabled states, all four editor audio tracks, trim
bounds, playback, simulated export and reduced motion. Screenshots and failed
traces are written to the ignored `test-results/` directory.

The replay demo offers Ctrl+Shift+F8/F9/F10 (default F10). Selection affects
only this demo, while focus is inside it; it does not configure a global
shortcut or persist settings. All editor tracks remain visible on mobile.
