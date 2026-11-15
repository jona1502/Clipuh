# Website app tour: interactive presentation of every app page

Date: 2026-11-15
Status: approved 2026-11-15

## Intent

The website presents the clipuh desktop app page by page: Capture, Library,
Editor and Settings. Each page gets a chapter on the homepage, and in each chapter
visitors can click around in an interactive recreation of that page. The four
design mockups in the repo root are the visual source:
`capture.png`, `library.png`, `editor.png` and `settings.png`.

Why: a visitor should come away feeling they have already used the app. That
does more than static screenshots or isolated widget demos.

## Goals

- One React recreation of the app window (sidebar, top capture bar, page area)
  that matches the mockups closely, using the existing design tokens.
- Four homepage chapters (`#capture`, `#library`, `#editor`, `#settings`). Each
  has explanatory copy plus the window opened on that page.
- Everything visible is clickable and responds: toggles, dropdowns, sliders,
  tabs, selection, filters, search, playback and trim.
- The window's sidebar switches pages inside the window and scrolls to the
  matching chapter.
- One shared state across all four chapters. Examples: favoriting a clip in the
  Library shows up in Recent Clips on Capture. Accent color and theme picked in
  Settings apply to every window. Replay toggle, resolution, FPS and length in
  the top bar stay in sync everywhere.

## Non-goals

- No real capture, audio, decoding or export. Everything is simulated with
  sample data.
- No new routes (`/features`, `/download`, ...). This spec covers the homepage
  only.
- No backend and no persistence beyond the page session.

## Anti-goals (failure even if it "works")

- Shows real game names, logos, HUDs or game footage (trademark risk). The
  mockups contain VALORANT, Fortnite, CS2, Apex and LoL. All of them get
  replaced with invented games and self-made thumbnails.
- Presents a planned feature as available. The honesty rules in
  `website/README.md` stay in force.
- Is a screenshot with hotspots instead of real UI. It has to feel like the app.
- Ships React on first paint, or makes the homepage feel heavy.

## Constraints

- Stack as is: Astro 7 static, React 19 islands, Tailwind 4 tokens from
  `src/styles/tokens.css`. No raw hex in components. No new runtime
  dependencies unless they are clearly justified.
- Islands mount with `client:visible`. `useReducedMotion()` is respected: no
  autoplay or looping meters under reduced motion.
- Accessibility: every control is a real button, input or listbox with a label
  and is keyboard operable. Focus stays inside the window. No keyboard traps.
- Widths 375, 430, 768, 1280, 1440 and 1920 must work, with no horizontal page
  scroll.
- `npm run check`, `npm run build` and `npm run test:browser` pass.

## Honesty / status handling (decided)

Everything is clickable, including features that are not implemented yet. A
control or panel that is not implemented carries a small "Planned" badge
(`statusBadgeClass("planned")`). Clicking it still gives visual feedback, and
the badge stays visible. The status of each mockup element must come from the
app code (`crates/`), not from guessing. `src/data/features.ts` is partly stale
(for example, multi-track editor audio has been implemented since it was
written), so the first step is a status audit that updates it.

### Mockup vs. app (decided by the user)

The mockups win. The site looks like the PNGs (layout, rows, options, tabs).
Wherever the real app differs or lacks an element, the element still appears
and carries the "Planned" badge. The status audit decides only which badge
each element gets. It does not change any layout.

## Visual direction (decided by the user: "like raycast.com")

Use raycast.com as the reference for polish and pacing, without copying its
assets, copy or brand:

- Near-black canvas with a strong hero: big headline, a soft accent glow behind
  a large floating product window, and a subtle reflection or shadow under it.
- Every chapter centers a large product window, with a short punchy headline
  above it ("Never miss the moment."). Feature points sit as small text blocks
  or a tab row under or beside the window, not as long bullet lists.
- Glass-effect panels (backdrop blur, 1px light top border, inner highlight)
  for the chapter backdrops, tinted with the accent.
- A tab row above a demo where useful (Library: All / Favorites / ...), in the
  style of Raycast's category tabs.
- Generous whitespace, tight letter-spacing on headlines, and gradient text
  (white to secondary) on chapter headlines.
- Scroll-in reveal (existing `.reveal`), restrained. No parallax gimmicks.
- Final CTA over a 3D-feeling accent shape (CSS or SVG, self-made).
- Only tokens from `tokens.css`. Add new tokens for glass and glow there if
  needed.

## Approaches considered

- **A. One shared app, scroll chapters (chosen).** One `AppShell` with four page
  components and a shared store. Each chapter mounts the window on its page.
  Closest to "presenting the app". Shared state makes it feel real. The existing
  demos are absorbed into it.
- B. Four independent demos. Simpler and fully parallel, but no shared state.
  It feels like four pictures, not one app.
- C. A separate `/demo` route with a full-screen app, homepage unchanged.
  Missing the page-by-page presentation the user asked for.

## Design

### Structure

```
src/components/interactive/app/
  store.tsx          AppStore: React context + reducer, mock data, actions
  mockData.ts        invented games, clips, audio sources, settings defaults
  thumbs.tsx         generated clip thumbnails (SVG/gradient scenes, no real games)
  AppShell.tsx       window chrome, sidebar, top capture bar, storage meter
  AppTour.tsx        island root: <AppStore> + <AppShell initialPage=...>
  pages/CapturePage.tsx
  pages/LibraryPage.tsx
  pages/EditorPage.tsx
  pages/SettingsPage.tsx
  ui/                Dropdown, Toggle, Slider, IconButton, Meter, Planned badge
```

Sharing state across islands: each chapter is its own `client:visible` island.
The islands share a store through a small module-level external store
(`useSyncExternalStore`), so state survives across all four. One single
full-page island is not an option, because it would hydrate everything up
front.

### Page behaviour (mockup → interaction)

- **Top bar (all pages):** source dropdown, resolution, FPS and replay length
  dropdowns, hotkey chip, "Replay active" toggle (the status text and color
  follow the toggle).
- **Capture:** change source (list of invented games, plus monitor and window
  options). The Storage & Quality estimate recomputes from resolution, FPS,
  length and codec. Recent Clips shows the 3 newest clips from the store, play
  opens the Editor chapter with that clip, and "View All Clips" goes to the
  Library. Audio mixer: live meters (static under reduced motion), gain slider
  with a dB readout, mute, solo, and "Add Audio Source".
- **Library:** tabs (All / Favorites / Game Clips / Recordings), search
  (Ctrl+F while focus is inside the window), sort, grid/list toggle,
  multi-select with a bottom action bar, per-card favorite, delete (with undo
  toast) and "open in editor". The count and size line updates.
- **Editor:** a clip loaded from the store. The preview shows the generated
  thumbnail scene. Play/pause, seek, ±10 s skip and a scrubber. Timeline with
  one video track and three audio tracks: drag trim handles, playhead, zoom,
  and per-track lock, visibility and mute. Side panel tabs Trim / Effects /
  Audio / Text / Export, with start/end/length fields synced to the handles.
  Split, delete and freeze. Export shows a simulated progress bar. Reuses the
  logic from the current `EditorDemo`, `editor/model.ts` and `Waveform.tsx`.
- **Settings:** the category list scrolls or highlights the matching panel.
  Video, audio, replay, hotkeys (record a new combination, scoped to the
  window), storage, and appearance (theme Dark / Light / System, accent
  swatches with a live preview). Changing the accent recolors all four windows.
  The light theme applies inside the windows only, never to the site.

### Homepage

Hero → four chapters (each `FeatureSection`-style: text + `DemoFrame` with the
label "Interactive demo · sample data") → Performance → More features → Final
CTA. The old Replay, Mixer and Editor sections and their islands get removed
once the new pages cover them. The hero `AppPreview` shows a static render of
the Capture page (an exported image of the recreation, or the island itself on
`client:idle`; builder's call based on LCP).

### Responsive

From `lg` the window renders at desktop layout and scales down via container
queries. Below `md` each page has a simplified stacked layout (sidebar becomes a
top tab row, grids collapse to 1 or 2 columns, timeline track headers become
compact). Nothing gets scaled with `transform: scale` to unreadable sizes.

### Tests

Playwright `tests/showcase.spec.ts` is rewritten for the tour. It covers:
sidebar navigation, shared state (favorite and accent across chapters), each
page's main interactions, Planned badges, reduced motion, all QA widths, and no
JavaScript (chapters still show copy).

## Build order (for subagents)

1. Status audit of the mockup elements against `crates/`, then update `features.ts`.
2. Store, mock data, thumbnails, shared UI primitives, AppShell + top bar.
3. Four pages in parallel (one subagent each, separate files).
4. Homepage integration, removal of the old sections, tests, README update.
5. Review pass.

## Calls made after the builder check (author's calls, correctable)

- **Navigation:** each window keeps its own current page. Its initial page is
  the chapter's page. The sidebar switches only that window and never scrolls
  the site. Cross-links (Recent Clips play, View All Clips, Open in Editor)
  switch the same window. The shared store holds data and settings, never the
  current page. This supersedes "scrolls to the matching chapter" above.
- **JS budget:** each page component is lazy-loaded (`React.lazy`) once its
  window needs it. The shared store and shell form one small chunk. Target:
  ≤150 KB gzip loaded when the first chapter becomes visible. Measure it in the
  build output.
- **Numbers and brands:** no hardware or vendor brand names (use "GPU encoder"
  and "USB microphone"). Sample numbers stay, but the storage estimate is
  labelled "Estimate" and computed as bitrate × length. No speed or
  performance claims.
- **Accent:** recolors the app windows only. It is applied by overriding the
  `--color-accent*` tokens on the window root. Each swatch defines its own
  `accent-foreground`. The site chrome keeps the brand accent.

## Left to the builder

- Exact invented game names and thumbnail art style (must read as games, not
  copies of real ones).
- Hero preview technique (static image vs island).
- Copy wording per chapter (English, matching the current site voice).
