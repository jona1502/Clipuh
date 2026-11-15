import { expect, test, type Locator, type Page } from "@playwright/test";
import { isPlanned } from "../src/data/appStatus";

test.use({ viewport: { width: 1440, height: 960 } });

/** Scrolls a chapter into view, waits for hydration and the lazy page. */
async function chapterWindow(page: Page, id: string) {
  const section = page.locator(`#${id}`);
  await section.scrollIntoViewIfNeeded();
  await expect(section.locator("astro-island").first()).not.toHaveAttribute(
    "ssr",
  );
  return section.locator("[data-app-window]");
}

async function editorWindow(page: Page) {
  await page.goto("/");
  const win = await chapterWindow(page, "editor");
  await expect(win.locator("[data-editor-page]")).toBeVisible();
  return win;
}

/** "01:02.300" -> 623 (tenths of a second). */
function parseClock(text: string): number {
  const m = /(\d+):(\d+)\.(\d)/.exec(text);
  if (!m) throw new Error(`not a clock value: ${text}`);
  return Number(m[1]) * 600 + Number(m[2]) * 10 + Number(m[3]);
}

const field = (win: Locator, name: string) =>
  win.getByRole("textbox", { name, exact: true });

async function fieldTenths(win: Locator, name: string) {
  return parseClock(await field(win, name).inputValue());
}

async function expectField(win: Locator, name: string, value: number) {
  await expect.poll(() => fieldTenths(win, name)).toBe(value);
}

test("play/pause: space toggles playback and the time advances", async ({
  page,
}) => {
  const win = await editorWindow(page);
  const time = win.locator("[data-editor-time]");
  const before = parseClock((await time.textContent()) ?? "");
  await win.locator("[data-editor-preview]").click();
  await page.keyboard.press("Space");
  await expect(win.getByRole("button", { name: "Pause" })).toBeVisible();
  await expect
    .poll(async () => parseClock((await time.textContent()) ?? ""))
    .toBeGreaterThan(before);
  await page.keyboard.press("Space");
  await expect(win.getByRole("button", { name: "Play" })).toBeVisible();
});

test("play/pause: reduced motion never auto-advances, skip still works", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  const win = await editorWindow(page);
  const time = win.locator("[data-editor-time]");
  const before = parseClock((await time.textContent()) ?? "");
  await win.locator("[data-editor-preview]").click();
  await page.keyboard.press("Space");
  await page.waitForTimeout(700);
  expect(parseClock((await time.textContent()) ?? "")).toBe(before);
  await win.getByRole("button", { name: "Forward 10 seconds" }).click();
  await expect
    .poll(async () => parseClock((await time.textContent()) ?? ""))
    .toBe(before + 100);
  await win.getByRole("button", { name: "Back 10 seconds" }).click();
  await expect
    .poll(async () => parseClock((await time.textContent()) ?? ""))
    .toBe(before);
});

test("trim handles: drag, arrow keys and the minimum length", async ({
  page,
}) => {
  const win = await editorWindow(page);
  const handle = win.getByRole("slider", { name: "Video start" });
  const box = await handle.boundingBox();
  if (!box) throw new Error("no handle box");
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width / 2 + 120, box.y + box.height / 2, {
    steps: 6,
  });
  await page.mouse.up();

  await expect.poll(() => fieldTenths(win, "Start")).toBeGreaterThan(0);
  const start = await fieldTenths(win, "Start");
  const end = await fieldTenths(win, "End");
  expect(start).toBeGreaterThan(0);
  await expectField(win, "Length", end - start);
  await expect(handle).toHaveAttribute("aria-valuenow", String(start));

  await handle.focus();
  await page.keyboard.press("ArrowRight");
  await expectField(win, "Start", start + 1);
  await page.keyboard.press("ArrowLeft");
  await page.keyboard.press("ArrowLeft");
  await expectField(win, "Start", start - 1);

  // Dragging far past the end stops MIN_TRIM (1 s) before it.
  const box2 = await handle.boundingBox();
  if (!box2) throw new Error("no handle box");
  await page.mouse.move(box2.x + box2.width / 2, box2.y + box2.height / 2);
  await page.mouse.down();
  await page.mouse.move(box2.x + 4000, box2.y + box2.height / 2, { steps: 4 });
  await page.mouse.up();
  await expectField(win, "Start", end - 10);
  await page.keyboard.press("ArrowRight");
  await expectField(win, "Start", end - 10);
});

test("fields → handles: typing an End value moves the handle", async ({
  page,
}) => {
  const win = await editorWindow(page);
  const end = field(win, "End");
  await end.fill("00:45.000");
  await end.press("Enter");
  await expect(win.getByRole("slider", { name: "Video end" })).toHaveAttribute(
    "aria-valuenow",
    "450",
  );
  await expectField(win, "Length", 450 - (await fieldTenths(win, "Start")));
});

test("track toggles flip aria-pressed and lock disables handles", async ({
  page,
}) => {
  const win = await editorWindow(page);
  for (const track of ["Video", "Game Audio", "Desktop Audio", "Microphone"]) {
    for (const verb of ["Lock", "Hide", "Mute"]) {
      const btn = win.getByRole("button", {
        name: `${verb} ${track}`,
        exact: true,
      });
      await expect(btn).toHaveAttribute("aria-pressed", "false");
      await btn.click();
      await expect(btn).toHaveAttribute("aria-pressed", "true");
      if (verb !== "Lock") {
        await btn.click();
        await expect(btn).toHaveAttribute("aria-pressed", "false");
      }
    }
  }
  const handle = win.getByRole("slider", { name: "Video start" });
  await expect(handle).toHaveAttribute("aria-disabled", "true");
  const before = await fieldTenths(win, "Start");
  await handle.focus();
  await page.keyboard.press("ArrowRight");
  await expectField(win, "Start", before);
  await expect(
    win.getByRole("slider", { name: "Microphone end" }),
  ).toHaveAttribute("aria-disabled", "true");
  if (isPlanned("editor.trackLock")) {
    await expect(
      win.locator('[data-element="editor.trackLock"] [data-planned-badge]'),
    ).toBeVisible();
  }
});

test("side tabs switch all five panels with Planned badges", async ({
  page,
}) => {
  const win = await editorWindow(page);
  const tabs = [
    ["Trim", null],
    ["Effects", "editor.tabEffects"],
    ["Audio", "editor.tabAudio"],
    ["Text", "editor.tabText"],
    ["Export", "editor.export"],
  ] as const;
  for (const [name, id] of tabs) {
    const tab = win.getByRole("tab", { name: new RegExp(`^${name}`) });
    await tab.click();
    await expect(tab).toHaveAttribute("aria-selected", "true");
    await expect(win.getByRole("tabpanel", { name })).toBeVisible();
    if (id && isPlanned(id)) {
      await expect(
        tab.locator("[data-planned-badge]").getByText("Planned"),
      ).toBeVisible();
    }
  }
});

test("rename: the pencil edits the title in the store", async ({ page }) => {
  const win = await editorWindow(page);
  const title = win.locator("[data-editor-title]");
  const old = (await title.textContent()) ?? "";
  await win.getByRole("button", { name: "Rename clip" }).click();
  const input = win.getByRole("textbox", { name: "Clip title" });
  await expect(input).toBeFocused();

  // Empty titles are rejected and keep the old title.
  await input.fill("   ");
  await input.press("Enter");
  await expect(title).toHaveText(old);

  await win.getByRole("button", { name: "Rename clip" }).click();
  await input.fill("Best round ever");
  await input.press("Enter");
  await expect(title).toHaveText("Best round ever");

  // Another window reads the same store (Library page lands at integration).
  const other = await chapterWindow(page, "capture");
  await other
    .getByRole("navigation", { name: "App pages" })
    .getByRole("button", { name: "Editor", exact: true })
    .click();
  await expect(other.locator("[data-editor-title]")).toHaveText(
    "Best round ever",
  );
});

test("export shows progress and a sample result", async ({ page }) => {
  const win = await editorWindow(page);
  await win.getByRole("button", { name: "Export clip" }).click();
  const bar = win.getByRole("progressbar", { name: "Export progress" });
  await expect(bar).toBeVisible();
  await expect(win.getByText("Exported (sample)")).toBeVisible();
  await expect(bar).toHaveAttribute("aria-valuenow", "100");
});

test("undo/redo reverts the last trim change", async ({ page }) => {
  const win = await editorWindow(page);
  const before = await fieldTenths(win, "Start");
  const handle = win.getByRole("slider", { name: "Video start" });
  await handle.focus();
  await page.keyboard.press("Shift+ArrowRight");
  await expectField(win, "Start", before + 10);
  await win.getByRole("button", { name: "Undo" }).click();
  await expectField(win, "Start", before);
  await win.getByRole("button", { name: "Redo" }).click();
  await expectField(win, "Start", before + 10);
});

test("narrow widths: no horizontal page scroll", async ({ page }) => {
  for (const width of [375, 768]) {
    await page.setViewportSize({ width, height: 900 });
    const win = await editorWindow(page);
    await expect(
      win.getByRole("slider", { name: "Video start" }),
    ).toBeVisible();
    const overflow = await page.evaluate(
      () => document.documentElement.scrollWidth - window.innerWidth,
    );
    expect(overflow).toBeLessThanOrEqual(0);
  }
});
