import { expect, test, type Page } from "@playwright/test";
import { INITIAL_CLIPS } from "../src/components/interactive/app/mockData";

/*
 * Cross-window checks: every chapter is its own island, all of them share
 * the module-level store. Each test hydrates the windows it reads by
 * scrolling them into view.
 */

test.use({ viewport: { width: 1440, height: 960 } });

const clips = INITIAL_CLIPS;

/** Scrolls a chapter into view, waits for hydration, returns its window. */
async function chapterWindow(page: Page, id: string) {
  const section = page.locator(`#${id}`);
  await section.scrollIntoViewIfNeeded();
  await expect(section.locator("astro-island").first()).not.toHaveAttribute(
    "ssr",
  );
  const win = section.locator("[data-app-window]");
  await expect(win).toBeVisible();
  return win;
}

test("favorite in Library shows a filled star in Capture recent clips", async ({
  page,
}) => {
  await page.goto("/");
  const capture = await chapterWindow(page, "capture");
  const row = capture.locator(`[data-recent-clip="${clips[0].id}"]`);
  await expect(row).toBeVisible();
  await expect(row.locator("[data-favorite-star]")).toHaveCount(0);

  const library = await chapterWindow(page, "library");
  const card = library.locator("[data-clip-id]").first();
  await expect(card).toHaveAttribute("data-clip-id", clips[0].id);
  await card.getByRole("button", { name: /^Favorite / }).click();

  await page.locator("#capture").scrollIntoViewIfNeeded();
  await expect(row.locator("[data-favorite-star]")).toBeVisible();
});

test("deleting the editor clip in Library loads the newest remaining clip", async ({
  page,
}) => {
  await page.goto("/");
  const editor = await chapterWindow(page, "editor");
  const title = editor.locator("[data-editor-title]");
  await expect(title).toHaveText(clips[0].title);

  const library = await chapterWindow(page, "library");
  await library
    .getByRole("button", { name: `Delete ${clips[0].title}`, exact: true })
    .click();
  await expect(library.locator("[data-library-toast]")).toBeVisible();

  await page.locator("#editor").scrollIntoViewIfNeeded();
  await expect(title).toHaveText(clips[1].title);
});

test("Open in Editor from Library shows that clip in the same window", async ({
  page,
}) => {
  await page.goto("/");
  const library = await chapterWindow(page, "library");
  const target = clips[2];
  await library
    .getByRole("checkbox", { name: `Select ${target.title}` })
    .click();
  await library
    .getByRole("button", { name: "Open in Editor", exact: true })
    .click();
  await expect(library).toHaveAttribute("data-page", "editor");
  await expect(library.locator("[data-editor-title]")).toHaveText(target.title);
});

test("rename in Editor shows the new title in the Library grid", async ({
  page,
}) => {
  await page.goto("/");
  const library = await chapterWindow(page, "library");
  const editor = await chapterWindow(page, "editor");
  await editor.getByRole("button", { name: "Rename clip" }).click();
  const input = editor.getByRole("textbox", { name: "Clip title" });
  await input.fill("Clutch on the ridge");
  await input.press("Enter");
  await expect(editor.locator("[data-editor-title]")).toHaveText(
    "Clutch on the ridge",
  );

  await page.locator("#library").scrollIntoViewIfNeeded();
  await expect(
    library.locator(`[data-clip-id="${clips[0].id}"] [data-clip-title]`),
  ).toHaveText("Clutch on the ridge");
});
