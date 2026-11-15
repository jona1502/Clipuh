import { expect, test, type Locator, type Page } from "@playwright/test";
import { INITIAL_CLIPS } from "../src/components/interactive/app/mockData";

test.use({ viewport: { width: 1440, height: 960 } });

const clips = INITIAL_CLIPS;
const gb = (mb: number) => (mb / 1024).toFixed(1);
const sum = (list: { sizeMb: number }[]) =>
  list.reduce((a, c) => a + c.sizeMb, 0);
const countLine = (list: { sizeMb: number }[]) =>
  `${list.length} clips • ${gb(sum(list))} GB`;
const titles = (list: { title: string }[]) => list.map((c) => c.title);

type Flags = { __prevented: boolean[] };

/** Opens the homepage and returns the hydrated Library chapter window. */
async function libraryWindow(page: Page) {
  await page.goto("/");
  const section = page.locator("#library");
  await section.scrollIntoViewIfNeeded();
  await expect(section.locator("astro-island").first()).not.toHaveAttribute(
    "ssr",
  );
  const win = section.locator("[data-app-window]");
  await expect(win.getByRole("tab", { name: "All Clips" })).toBeVisible();
  return win;
}

const cardTitles = (win: Locator) =>
  win.locator("[data-clip-id] [data-clip-title]").allTextContents();

test("tabs filter", async ({ page }) => {
  const win = await libraryWindow(page);
  const count = win.locator("[data-library-count]");
  await expect(count).toHaveText(countLine(clips));

  await win.getByRole("tab", { name: "Favorites" }).click();
  const favs = clips.filter((c) => c.favorite);
  await expect(count).toHaveText(countLine(favs));
  expect((await cardTitles(win)).sort()).toEqual(titles(favs).sort());

  await win.getByRole("tab", { name: "Recordings" }).click();
  const recs = clips.filter((c) => c.kind === "recording");
  await expect(count).toHaveText(countLine(recs));
  expect((await cardTitles(win)).sort()).toEqual(titles(recs).sort());

  await win.getByRole("tab", { name: "Game Clips" }).click();
  const games = clips.filter((c) => c.kind === "game");
  await expect(count).toHaveText(countLine(games));
  await expect(win.getByRole("tab", { name: "Game Clips" })).toHaveAttribute(
    "aria-selected",
    "true",
  );
});

test("search narrows the grid and Ctrl+F is scoped to the window", async ({
  page,
}) => {
  const win = await libraryWindow(page);
  const search = win.getByRole("searchbox", { name: "Search clips" });
  await search.fill("hollow ridge");
  const expected = clips.filter((c) => /hollow ridge/i.test(c.title));
  await expect(win.locator("[data-clip-id]")).toHaveCount(expected.length);
  await search.fill("zzzz");
  await expect(win.getByText("No clips match")).toBeVisible();
  await search.fill("");
  await expect(win.locator("[data-clip-id]")).toHaveCount(clips.length);

  await page.evaluate(() => {
    const w = window as unknown as Flags;
    w.__prevented = [];
    document.addEventListener("keydown", (e) => {
      if (e.ctrlKey && e.key.toLowerCase() === "f")
        w.__prevented.push(e.defaultPrevented);
    });
    (document.activeElement as HTMLElement | null)?.blur();
  });
  await page.keyboard.press("Control+f");
  await expect(search).not.toBeFocused();
  expect(
    await page.evaluate(() => (window as unknown as Flags).__prevented),
  ).toEqual([false]);

  await win
    .locator("[data-clip-id]")
    .first()
    .getByRole("button", { name: /^Play / })
    .focus();
  await page.keyboard.press("Control+f");
  await expect(search).toBeFocused();
  expect(
    await page.evaluate(() => (window as unknown as Flags).__prevented),
  ).toEqual([false, true]);
});

test("sort: Oldest reverses the order", async ({ page }) => {
  const win = await libraryWindow(page);
  const newest = await cardTitles(win);
  expect(newest[0]).toBe(titles(clips)[0]);
  await win.getByRole("button", { name: /^Sort/ }).click();
  await win.getByRole("option", { name: "Oldest" }).click();
  expect(await cardTitles(win)).toEqual([...newest].reverse());
});

test("grid and list toggle", async ({ page }) => {
  const win = await libraryWindow(page);
  const grid = win.getByRole("button", { name: "Grid view" });
  const list = win.getByRole("button", { name: "List view" });
  const body = win.locator("[data-library-view]");
  await expect(grid).toHaveAttribute("aria-pressed", "true");
  await expect(list).toHaveAttribute("aria-pressed", "false");
  await expect(body).toHaveAttribute("data-library-view", "grid");
  await list.click();
  await expect(list).toHaveAttribute("aria-pressed", "true");
  await expect(grid).toHaveAttribute("aria-pressed", "false");
  await expect(body).toHaveAttribute("data-library-view", "list");
  await expect(win.locator("[data-clip-id]")).toHaveCount(clips.length);
});

test("favorite toggles on a card", async ({ page }) => {
  const win = await libraryWindow(page);
  const first = win.locator("[data-clip-id]").first();
  const fav = first.getByRole("button", { name: /^Favorite / });
  await expect(fav).toHaveAttribute("aria-pressed", "false");
  await fav.click();
  await expect(fav).toHaveAttribute("aria-pressed", "true");
  await win.getByRole("tab", { name: "Favorites" }).click();
  await expect(win.locator("[data-clip-id]")).toHaveCount(
    clips.filter((c) => c.favorite).length + 1,
  );
});

test("multi-select bar, delete and undo", async ({ page }) => {
  const win = await libraryWindow(page);
  const cards = win.locator("[data-clip-id]");
  await cards.nth(0).getByRole("checkbox").click();
  await cards.nth(1).getByRole("checkbox").click();
  await expect(win.locator("[data-selection-summary]")).toHaveText(
    `2 selected • ${clips[0].sizeMb + clips[1].sizeMb} MB`,
  );
  await win.getByRole("button", { name: "Delete", exact: true }).click();
  await expect(cards).toHaveCount(clips.length - 2);
  const toast = win.locator("[data-library-toast]");
  await expect(toast).toContainText("2 clips deleted");
  await toast.getByRole("button", { name: "Undo" }).click();
  await expect(cards).toHaveCount(clips.length);
  expect(await cardTitles(win)).toEqual(titles(clips));
});

test("delete the editor clip", async ({ page }) => {
  const win = await libraryWindow(page);
  // The editor opens on the newest clip.
  await win.getByRole("button", { name: `Delete ${clips[0].title}` }).click();
  await expect(win.locator("[data-clip-id]")).toHaveCount(clips.length - 1);
  await expect(
    win.locator("[data-clip-id]").filter({ hasText: clips[0].title }),
  ).toHaveCount(0);
  // The Editor page is a placeholder in this branch: the title of the newest
  // remaining clip in the #editor window is asserted at integration.
  await expect(page.locator("#editor [data-app-window]")).toHaveAttribute(
    "data-page",
    "editor",
  );
});

test("delete all shows the empty state", async ({ page }) => {
  const win = await libraryWindow(page);
  await win.getByRole("checkbox", { name: "Select all clips" }).click();
  await expect(win.locator("[data-selection-summary]")).toHaveText(
    `${clips.length} selected • ${sum(clips)} MB`,
  );
  await win.getByRole("button", { name: "Delete", exact: true }).click();
  await expect(win.getByText("No clips yet")).toBeVisible();
  await expect(win.locator("[data-clip-id]")).toHaveCount(0);
  await win.getByRole("button", { name: "Undo" }).click();
  await expect(win.locator("[data-clip-id]")).toHaveCount(clips.length);
});

test("open in editor", async ({ page }) => {
  const win = await libraryWindow(page);
  const target = clips[2];
  await win.getByRole("checkbox", { name: `Select ${target.title}` }).click();
  await win
    .getByRole("button", { name: "Open in Editor", exact: true })
    .click();
  await expect(win).toHaveAttribute("data-page", "editor");
  // The title of the opened clip is asserted at integration (placeholder
  // Editor page in this branch).
});

test("card edit button opens the editor", async ({ page }) => {
  const win = await libraryWindow(page);
  await win.getByRole("button", { name: `Edit ${clips[3].title}` }).click();
  await expect(win).toHaveAttribute("data-page", "editor");
});
