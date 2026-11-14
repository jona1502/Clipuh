# clipuh website

Official website for clipuh. Static site built with Astro; fully isolated from
the Rust workspace (no Cargo involvement in either direction).

## Stack

- Astro 7 (static output, no adapter, no backend)
- TypeScript (strict)
- Tailwind CSS 4 via `@tailwindcss/vite`
- React 19 for interactive islands only

## Commands

Run from `/website` (Node >= 20, npm):

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
- `src/components/ui/` — buttons, badges, cards (`.astro`)
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
  Values mirror `crates/clipper-ui/src/theme/mod.rs`; keep them in sync.
- **Imports** use the `@/` alias for `src/`.
- `src/data/site.ts` is the single source for name, tagline and SEO defaults.
- Set `site` in `astro.config.mjs` once the production domain is known
  (enables canonical URLs and later a sitemap).

`IntegrationCheck.tsx` only proves the React integration works and is not used
on any page; replace it with the first real island.
