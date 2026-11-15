# Website App Tour Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rebuild the homepage as a Raycast-style tour of the clipuh app. Each of four chapters (Capture, Library, Editor, Settings) holds a clickable React recreation of that app page, built from the root mockups.

**Architecture:** There are four `client:visible` islands, one per chapter. Each renders the same `AppShell` with its own current page, so navigation stays local to the window. All windows share one module-level external store (`useSyncExternalStore`) for data and settings. Page components are `React.lazy` chunks.

**Tech Stack:** Astro 7 (static), React 19, Tailwind 4 tokens, Playwright, Vitest (new devDependency, store unit tests only).

**Spec:** `docs/superpowers/specs/2026-11-15-website-app-tour-design.md` (read it: visual direction, honesty rules, calls).

All paths below are relative to `website/` unless they start with `crates/` or `docs/`.

## Global Constraints

- Branch `feat/website-app-tour` off `main`. Never commit `clipuh_05_synth_check.wav`.
- Only semantic tokens from `src/styles/tokens.css` in components; no raw hex. New tokens (glass, glow, light window theme, accent swatches) go in `tokens.css` or a CSS file next to it.
- No real game names, logos, HUDs or footage. No hardware or vendor brands (no NVENC, HyperX, Elgato): use "GPU encoder" and "USB microphone".
- Every element listed in `src/data/appStatus.ts` that is `planned` shows `<PlannedBadge>` and stays clickable.
- Demo frames are labelled "Interactive demo · sample data".
- The storage estimate is labelled "Estimate" and computed as `bitrateMbps × replaySec / 8` MB. No performance claims.
- Accent and light theme apply only inside app windows (overrides on the window root element), never to the site chrome.
- `useReducedMotion()`: no looping meters, autoplay or animated playback under reduced motion. Controls still work.
- All controls are native buttons or inputs or ARIA listboxes with accessible names, and are keyboard operable. Window-scoped shortcuts (Ctrl+F, space) fire only while focus is inside that window.
- QA widths 375, 430, 768, 1024, 1280, 1440, 1920 have no horizontal page scroll.
- JS loaded when the first chapter becomes visible: ≤150 KB gzip (shell + store + Capture page + React).
- Every task ends green: `npm run check`, `npm run build`, `npx vitest run`, and `npm run test:browser` (once specs exist).
- Copy is in English, in the existing site voice.

## Review Focus

1. **Shared state across windows:** a favorite toggled in the Library window shows in Capture's Recent Clips without a reload. Pinned in Task 4.
2. **Deleting the clip that is open in the Editor:** the Editor falls back to the newest remaining clip and does not crash. With 0 clips left, every window shows an empty state. Pinned in Task 2 (reducer) and Task 5.
3. **Keyboard shortcuts leaking:** Ctrl+F or space pressed outside any window must not be hijacked (the browser's find and page scroll keep working). Pinned in Task 5.
4. **Hotkey recording conflict:** recording a combo that another action already uses is rejected with a message, and Escape cancels recording. Pinned in Task 2 (reducer) and Task 7.
5. **Narrow widths:** at 375px the window switches to the stacked layout, and the timeline and grid scroll inside the window, never the page. Pinned in Task 8.

---

### Task 1: Status audit of mockup elements

**Files:**
- Create: `src/data/appStatus.ts`
- Modify: `src/data/features.ts` (fix stale statuses and copy, e.g. editor multitrack)
- Test: `src/data/appStatus.test.ts`

**Interfaces:**
- Produces: `type ElementId` (union of the ids below), `const elementStatus: Record<ElementId, FeatureStatus>`, `function isPlanned(id: ElementId): boolean`.

Element ids (exactly these):
`topbar.sourcePicker topbar.resolution topbar.fps topbar.replayLength topbar.hotkey topbar.replayToggle capture.changeSource capture.storageEstimate capture.codec capture.recentClips mixer.gameAudio mixer.desktopAudio mixer.microphone mixer.meters mixer.gain mixer.mute mixer.solo mixer.addSource library.grid library.listView library.favorites library.tabGameClips library.tabRecordings library.search library.filters library.sort library.multiSelect library.delete library.openFolder library.openInEditor editor.rename editor.playback editor.trim editor.multitrack editor.trackLock editor.trackVisibility editor.trackMute editor.split editor.deleteSegment editor.freeze editor.tabEffects editor.tabAudio editor.tabText editor.export editor.undoRedo editor.snap editor.zoom editor.addTrack settings.resolution settings.fps settings.encoder settings.bitrate settings.audioDevices settings.separateTracks settings.replayDuration settings.bufferLocation settings.autoStartReplay settings.hotkeySaveClip settings.hotkeyRecording settings.hotkeyOpenApp settings.clipLocation settings.retention settings.autoCleanup settings.theme settings.accent sidebar.storageMeter`

- [ ] **Step 1:** Create the branch. Add Vitest: `npm i -D vitest`. Add the script `"test:unit": "vitest run"` and a `vitest.config.ts` that reuses the `@/` alias and the `src/**/*.test.ts(x)` include.
- [ ] **Step 2: Write the failing test** `appStatus.test.ts`. It asserts: every id listed above is a key of `elementStatus` (hardcode the list in the test), the key count matches the list, and every value is `"planned"` or `"in-development"`.
- [ ] **Step 3:** Run `npx vitest run src/data/appStatus.test.ts`. Expected: FAIL (module missing).
- [ ] **Step 4: Audit.** For each id, search `crates/` (notably `crates/clipper-ui/src/views/**`, `state.rs`, `crates/clipper-editor`, `clipper-storage`, `clipper-audio`) for the behavior. Mark it `in-development` only if the app implements it, otherwise `planned`. Put a one-line comment per id naming the evidence (file path) or "not found". Update `features.ts` copy and statuses that contradict the findings.
- [ ] **Step 5:** Run the test. Expected: PASS. Then run `npm run check`.
- [ ] **Step 6: Commit** `feat(website): audit app element status for the app tour`.

### Task 2: Store, mock data, thumbnails

**Files:**
- Create: `src/components/interactive/app/store.ts`, `mockData.ts`, `thumbs.tsx`, `store.test.ts`

**Interfaces:**
- Consumes: none (Task 1 is independent).
- Produces (all from `store.ts` unless noted):
  ```ts
  type PageId = "capture" | "library" | "editor" | "settings";
  type Resolution = "720p" | "1080p" | "1440p" | "2160p";
  type Fps = 30 | 60 | 120 | 144;
  type ReplaySec = 15 | 30 | 60 | 120 | 300;
  type Codec = "h264" | "hevc" | "av1";
  type HotkeyAction = "saveClip" | "recording" | "openApp";
  type AccentId = "violet" | "blue" | "cyan" | "green" | "yellow" | "orange" | "pink";
  interface Game { id: string; name: string; scene: ThumbScene }          // mockData.ts
  interface Clip { id: string; title: string; gameId: string; kind: "game" | "recording";
                   minutesAgo: number; durationSec: number; sizeMb: number; favorite: boolean }
  interface AudioSource { id: string; kind: "game" | "desktop" | "mic" | "extra";
                          label: string; detail: string; gainDb: number; muted: boolean; solo: boolean }
  interface CaptureState { sourceId: string; resolution: Resolution; fps: Fps; replaySec: ReplaySec;
                           codec: Codec; replayActive: boolean }
  interface SettingsState { theme: "dark" | "light" | "system"; accent: AccentId; bitrateMbps: number;
                            encoder: string; bufferLocation: "ram" | "disk" | "hybrid";
                            autoStartReplay: boolean; separateTracks: boolean; clipLocation: string;
                            keepDays: 7 | 14 | 30 | 90 | 0; autoCleanup: boolean;
                            hotkeys: Record<HotkeyAction, string>;
                            devices: { game: string; desktop: string; mic: string } }
  interface AppState { capture: CaptureState; settings: SettingsState; clips: Clip[];
                       sources: AudioSource[]; editorClipId: string | null;
                       storage: { usedGb: number; totalGb: number }; lastError: string | null }
  type Action =
    | { type: "capture/set"; patch: Partial<CaptureState> }
    | { type: "settings/set"; patch: Partial<SettingsState> }
    | { type: "hotkey/set"; action: HotkeyAction; combo: string }
    | { type: "clip/toggleFavorite"; id: string }
    | { type: "clips/delete"; ids: string[] }
    | { type: "clips/restore"; clips: Clip[] }
    | { type: "clip/rename"; id: string; title: string }
    | { type: "editor/open"; id: string }
    | { type: "source/set"; id: string; patch: Partial<Pick<AudioSource, "gainDb" | "muted" | "solo">> }
    | { type: "source/add" }
    | { type: "error/clear" };
  function reducer(state: AppState, action: Action): AppState;
  function createInitialState(): AppState;
  const appStore: { getState(): AppState; dispatch(a: Action): void; subscribe(l: () => void): () => void; reset(): void };
  function useApp<T>(selector: (s: AppState) => T): T;      // useSyncExternalStore over appStore
  const dispatch: (a: Action) => void;
  function estimateClipMb(replaySec: number, bitrateMbps: number): number; // = round(bitrate*sec/8)
  function visibleClips(state: AppState): Clip[];            // newest first
  const ACCENTS: Record<AccentId, { label: string; tokenVar: string }>; // CSS vars from Task 3
  // thumbs.tsx
  type ThumbScene = "canyon" | "neon" | "forest" | "fortress" | "arena" | "desert";
  function ClipThumb(props: { scene: ThumbScene; seed: number; className?: string }): JSX.Element; // inline SVG, deterministic by seed
  ```
- Initial data mirrors the mockups: 6 invented games (names are your call; they must not resemble real titles), ≥12 clips with the mockup-like durations and sizes, 3 sources (game −12 dB, desktop −18 dB, mic −6 dB), `replayActive: true`, `1080p`, `60`, `60`, `h264`, bitrate 20, hotkeys `Alt + S` / `Alt + R` / `Ctrl + Shift + C`, storage 357.2/500 GB, `editorClipId` = newest clip.

- [ ] **Step 1: Write failing tests** in `store.test.ts` (call `appStore.reset()` in `beforeEach`):
  - `toggleFavorite flips only that clip`
  - `estimateClipMb(60, 20) === 150`
  - `delete of the open editor clip moves editorClipId to newest remaining; deleting all sets null`
  - `hotkey/set rejects a combo used by another action: hotkeys unchanged, lastError set`
  - `solo on one source leaves others' solo untouched; mute toggles independently`
  - `subscribe listener fires once per dispatch and stops after unsubscribe`
  - `source/add appends an "extra" source with a unique id`
  - `clips/restore re-inserts deleted clips, ordered by minutesAgo`
- [ ] **Step 2:** Run `npx vitest run src/components/interactive/app`. Expected: FAIL.
- [ ] **Step 3:** Implement `store.ts`, `mockData.ts` and `thumbs.tsx` with the interfaces above. Thumbnails are layered SVG shapes per scene (sky gradient, silhouettes, a light source), colored with token vars, no images.
- [ ] **Step 4:** Run the tests. Expected: PASS. Then `npm run check`.
- [ ] **Step 5: Commit** `feat(website): shared app-tour store, mock data and clip thumbnails`.

### Task 3: AppShell, UI primitives, chapter islands, homepage wiring

**Files:**
- Create: `src/components/interactive/app/ui/{Dropdown,Toggle,Slider,IconButton,Meter,PlannedBadge,Icon}.tsx`, `AppShell.tsx`, `TopBar.tsx`, `Sidebar.tsx`, `AppTour.tsx`, `pages/{Capture,Library,Editor,Settings}Page.tsx` (placeholders), `src/styles/app-window.css`
- Create: `src/components/showcase/TourChapter.astro`
- Modify: `src/pages/index.astro` (add four chapters after the Hero; old sections stay until Task 7), `src/data/features.ts` (chapter copy `tourChapters`)
- Test: `tests/tour-shell.spec.ts`

**Interfaces:**
- Consumes: Task 2 store; Task 1 `isPlanned`, `ElementId`.
- Produces:
  ```ts
  interface PageProps { navigate: (p: PageId) => void }
  function AppTour(props: { initialPage: PageId }): JSX.Element  // island root, default export
  function PlannedBadge(props: { id: ElementId }): JSX.Element | null   // null when not planned
  function Dropdown<T extends string | number>(p: { label: string; value: T; options: { value: T; label: string }[];
     onChange(v: T): void; plannedId?: ElementId; className?: string }): JSX.Element  // ARIA listbox
  function Toggle(p: { label: string; checked: boolean; onChange(v: boolean): void }): JSX.Element
  function Slider(p: { label: string; min: number; max: number; step: number; value: number;
     onChange(v: number): void; format?(v: number): string }): JSX.Element   // native range, .demo-slider
  function Meter(p: { level: number /*0..1*/; animated: boolean }): JSX.Element
  function Icon(p: { name: IconName; className?: string }): JSX.Element // inline SVG set covering the mockup icons
  ```
  The window root is `<div data-app-window data-page={page} data-theme={resolvedTheme} style={accent vars}>`. Sidebar buttons have accessible names "Capture", "Library", "Editor", "Settings" and `aria-current="page"` on the active one. `TourChapter.astro` props are `{ id: PageId; copy: ChapterCopy }` and it mounts `<AppTour client:visible initialPage={id} />` inside `DemoFrame` (label "Interactive demo · sample data"). Section ids are `capture`, `library`, `editor`, `settings`.
- Window themes: `app-window.css` defines `[data-app-window][data-theme="light"]` surface/text tokens mirroring `Base::LIGHT` in `crates/clipper-ui/src/theme/palette.rs:145`, and one `[data-accent="<id>"]` block per `AccentId` setting `--color-accent` and `--color-accent-foreground` (white or `#111118`, whichever has higher WCAG contrast). Mockup swatch order: violet, blue, cyan, green, yellow, orange, pink.
- Each page is loaded via `React.lazy(() => import("./pages/XPage"))` inside `<Suspense>` with a skeleton the same size as the page.

- [ ] **Step 1: Write failing Playwright tests** in `tour-shell.spec.ts` (desktop 1440px):
  - `each chapter window opens on its own page`: `#capture [data-app-window]` has `data-page="capture"`, and the same holds for the other three.
  - `sidebar switches only its own window`: click "Library" in the `#capture` window. That window's `data-page` becomes `library`, `#library`'s window stays `library`, and `window.scrollY` is unchanged.
  - `replay toggle syncs across windows`: turn the toggle off in `#capture` and scroll to `#settings`. That window's toggle is unchecked and its status text reads "Replay paused".
  - `planned badge shown`: for any id `isPlanned` returns true for that is rendered in the shell (e.g. the top bar), the badge text "Planned" is visible next to it.
- [ ] **Step 2:** Run `npm run build && npx playwright test tests/tour-shell.spec.ts`. Expected: FAIL.
- [ ] **Step 3:** Implement the primitives, Sidebar (logo, 4 nav items, storage meter), TopBar (the top bar row from the mockups, with "Replay active"/"Replay paused" in success/muted color), AppShell (desktop layout from `lg`; below `md` the sidebar becomes a top tab row), AppTour, placeholder pages (a heading only), TourChapter, and the homepage wiring. Desktop window proportions follow the mockups (~16:9) via container queries.
- [ ] **Step 4:** Run the tests. Expected: PASS. Run `npm run check`.
- [ ] **Step 5: Measure** the JS budget: `npm run build` and `npx vite-bundle-visualizer` or `gzip -c dist/_astro/<chunk>.js | wc -c` for the chunks the Capture island loads. Record the number in the commit body. It must be ≤150 KB.
- [ ] **Step 6: Commit** `feat(website): app window shell and four tour chapters`.

### Tasks 4–7 in parallel (one subagent each; each touches only its own page file, a `pages/<page>/` subfolder and its own spec)

Shared rules for 4–7: match the mockup layout of that page. Wrap each element from Task 1's list with `<PlannedBadge id>`. Navigate through `PageProps.navigate`. Read and write only through `useApp` and `dispatch`. Scope every keyboard handler to the window root. Planned elements still give visual feedback when clicked.

### Task 4: Capture page

**Files:** Modify `pages/CapturePage.tsx`; create `pages/capture/*`; test `tests/tour-capture.spec.ts`.
**Interfaces:** consumes the store, primitives and `ClipThumb`. Produces nothing new.

- [ ] **Step 1: Failing tests:**
  - `source change`: pick another game in the Capture Source card. The card and the top bar both show the new name.
  - `estimate updates`: change replay length to 120 s in the top bar. The estimate reads `≈ 300 MB / Clip` (bitrate 20) and carries the label "Estimate".
  - `gain slider`: press ArrowRight on the Microphone gain slider. The dB readout increases by the step.
  - `mute/solo pressed state`: `aria-pressed` flips on click.
  - `recent clip play opens editor in same window`: click play on the first recent clip. The window's `data-page` becomes `editor`.
  - `View All Clips`: the window's `data-page` becomes `library`.
  - `Add Audio Source`: a fourth mixer row appears.
  - `favorite syncs`: dispatching a favorite from the `#library` window (star on first card) shows a filled star on that clip in `#capture` Recent Clips.
  - `reduced motion`: with `reducedMotion: "reduce"`, the meter bars do not change over 1 s.
- [ ] **Step 2:** Run. Expected: FAIL. **Step 3:** Implement. **Step 4:** Run. Expected: PASS, plus `npm run check`.
- [ ] **Step 5: Commit** `feat(website): interactive Capture page`.

### Task 5: Library page

**Files:** Modify `pages/LibraryPage.tsx`; create `pages/library/*`; test `tests/tour-library.spec.ts`.

- [ ] **Step 1: Failing tests:**
  - `tabs filter`: Favorites shows only favorited clips, Recordings only `kind: "recording"`, and the count line `N clips • X GB` updates.
  - `search`: typing a game name narrows the grid. `Ctrl+F` with focus in the window focuses search. `Ctrl+F` with focus on the page body is not prevented (`defaultPrevented === false` in a page-level listener).
  - `sort`: "Oldest" reverses the order.
  - `grid/list toggle`: `aria-pressed` and layout switch.
  - `multi-select bar`: selecting 2 cards shows "2 selected • <sum> MB". Delete removes them and an Undo toast restores them.
  - `delete the editor clip`: delete the clip open in the editor. The `#editor` window shows the newest remaining clip's title.
  - `delete all → empty state`: select all and delete. The empty-state text "No clips yet" is visible.
  - `open in editor`: the window's `data-page` becomes `editor` with that clip's title.
- [ ] **Step 2–4:** as in Task 4.
- [ ] **Step 5: Commit** `feat(website): interactive Library page`.

### Task 6: Editor page

**Files:** Modify `pages/EditorPage.tsx`; create `pages/editor/*`; reuse `src/components/interactive/editor/{model.ts,Waveform.tsx}` (import, do not move). Test `tests/tour-editor.spec.ts`.

- [ ] **Step 1: Failing tests:**
  - `play/pause`: space with focus in the window toggles play, and the time readout advances. Under reduced motion there is no auto-advance, but stepping with ±10 s works.
  - `trim handles`: drag the video start handle right. The Start field updates and the Length field equals End − Start. Arrow keys on a handle move it by 0.1 s. Start can never pass End − MIN_TRIM.
  - `fields → handles`: typing in the End field moves the handle.
  - `track toggles`: lock, eye and speaker per track flip `aria-pressed`. A locked track's handles are disabled.
  - `side tabs`: all 5 tabs switch panels. Effects and Text show the Planned badge if Task 1 marks them planned.
  - `rename`: the pencil allows editing the title, and Enter dispatches `clip/rename`. The `#library` window shows the new title.
  - `export`: click Export. A progress bar reaches done and the text "Exported (sample)" appears.
  - `undo/redo`: undo reverts the last trim change.
- [ ] **Step 2–4:** as in Task 4.
- [ ] **Step 5: Commit** `feat(website): interactive Editor page`.

### Task 7: Settings page

**Files:** Modify `pages/SettingsPage.tsx`; create `pages/settings/*`; test `tests/tour-settings.spec.ts`.

- [ ] **Step 1: Failing tests:**
  - `category nav`: click "Hotkeys" in the left list. The Hotkeys panel is scrolled into view inside the window, the list item is marked current, and `window.scrollY` is unchanged.
  - `accent recolors all windows`: click the green swatch. Every `[data-app-window]` has `data-accent="green"`, and the site header's download button color is unchanged.
  - `theme light`: Light sets `data-theme="light"` on all windows. `body` background is unchanged.
  - `hotkey record`: click the pencil on "Start / Stop Recording" and press Alt+K. The row shows `Alt + K`. Recording `Alt + S` instead shows the conflict error and keeps `Alt + R`. Escape cancels.
  - `resolution sync`: set Resolution to 1440p. The top bar in `#capture` shows `1440p`.
  - `checkbox`: "Separate audio tracks" toggles `checked`.
- [ ] **Step 2–4:** as in Task 4.
- [ ] **Step 5: Commit** `feat(website): interactive Settings page`.

### Task 8: Raycast-style homepage, cleanup, docs

**Files:**
- Modify: `src/pages/index.astro`, `src/components/sections/Hero.astro`, `src/components/showcase/{TourChapter,PerformanceSection,MoreFeatures,FinalCta}.astro`, `src/styles/{tokens,global}.css`, `src/components/layout/Header.astro` (nav links to the chapters), `README.md`
- Delete: `src/components/interactive/{ReplayDemo,MixerDemo,EditorDemo}.tsx`, `src/components/showcase/{ReplaySection,MixerSection,EditorSection,FeatureIntro}.astro`, `tests/showcase.spec.ts`, and `AppPreview.astro` if the hero no longer uses it
- Create: `tests/tour-page.spec.ts`

**Interfaces:** consumes `TourChapter`. Produces the final page.

- [ ] **Step 1: Failing tests** in `tour-page.spec.ts`. For each width in `[375, 430, 768, 1024, 1280, 1440, 1920]`:
  - no console errors and no 4xx responses
  - `document.documentElement.scrollWidth <= innerWidth`
  - all four chapter headings are visible after scrolling
  - at 375px the window uses the stacked layout (sidebar as a tab row), and the Editor timeline scrolls horizontally inside the window
  - the header nav links reach each chapter (the mobile menu is used below 768px)

  Plus, without JavaScript (`javaScriptEnabled: false`), the chapter headings and copy are visible.
- [ ] **Step 2:** Run. Expected: FAIL (old sections, no nav links).
- [ ] **Step 3:** Implement the visual direction from the spec:
  - Hero: big headline, accent glow, and the Capture window floating with a reflection. Use the `AppTour` island with `client:idle` if LCP stays text, otherwise a static rendering; check with a Lighthouse run in the commit body.
  - Chapters: a punchy gradient headline above a large centered window on a glass backdrop, and 3 short feature blocks below.
  - Keep Performance and MoreFeatures, restyled as glass cards.
  - FinalCta over a self-made CSS/SVG 3D accent shape.
  - Delete the old demos and sections.
  - Update the README (structure, islands, honesty rules, tests, JS budget).
- [ ] **Step 4:** Run `npm run check && npm run build && npx vitest run && npm run test:browser`. Expected: all PASS.
- [ ] **Step 5: Commit** `feat(website): Raycast-style app tour homepage`.

### Task 9: Whole-branch review

- [ ] Run `superpowers:requesting-code-review` against `main...feat/website-app-tour`, focused on the Review Focus list, the honesty rules, accessibility and the JS budget. Fix the findings, then re-run all four commands.
