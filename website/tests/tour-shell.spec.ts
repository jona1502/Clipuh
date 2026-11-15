import { expect, test, type Page } from "@playwright/test";
import { isPlanned } from "../src/data/appStatus";
import { SHELL_ELEMENT_IDS } from "../src/components/interactive/app/shellElements";

const pages = ["capture", "library", "editor", "settings"] as const;

test.use({ viewport: { width: 1440, height: 960 } });

/** Scrolls the chapter into view and waits until its island has hydrated. */
async function chapterWindow(page: Page, id: string) {
  const section = page.locator(`#${id}`);
  await section.scrollIntoViewIfNeeded();
  await expect(section.locator("astro-island").first()).not.toHaveAttribute(
    "ssr",
  );
  return section.locator("[data-app-window]");
}

test("each chapter window opens on its own page", async ({ page }) => {
  await page.goto("/");
  for (const id of pages) {
    await expect(page.locator(`#${id} [data-app-window]`)).toHaveAttribute(
      "data-page",
      id,
    );
  }
});

test("sidebar switches only its own window", async ({ page }) => {
  await page.goto("/");
  const capture = await chapterWindow(page, "capture");
  const before = await page.evaluate(() => window.scrollY);
  await capture
    .getByRole("navigation", { name: "App pages" })
    .getByRole("button", { name: "Library", exact: true })
    .click();
  await expect(capture).toHaveAttribute("data-page", "library");
  await expect(
    capture
      .getByRole("navigation", { name: "App pages" })
      .getByRole("button", { name: "Library", exact: true }),
  ).toHaveAttribute("aria-current", "page");
  await expect(page.locator("#library [data-app-window]")).toHaveAttribute(
    "data-page",
    "library",
  );
  expect(await page.evaluate(() => window.scrollY)).toBe(before);
});

test("replay toggle syncs across windows", async ({ page }) => {
  await page.goto("/");
  const capture = await chapterWindow(page, "capture");
  const toggle = capture.getByRole("switch", { name: "Replay buffer" });
  await expect(toggle).toHaveAttribute("aria-checked", "true");
  await toggle.click();
  await expect(toggle).toHaveAttribute("aria-checked", "false");
  const settings = await chapterWindow(page, "settings");
  await expect(
    settings.getByRole("switch", { name: "Replay buffer" }),
  ).toHaveAttribute("aria-checked", "false");
  await expect(
    settings.getByText("Replay paused", { exact: true }),
  ).toBeVisible();
});

test("planned badge shown", async ({ page }) => {
  await page.goto("/");
  const win = await chapterWindow(page, "capture");
  const shell = win.locator("[data-app-shell-chrome]");
  await expect(shell.first()).toBeVisible();
  const planned = SHELL_ELEMENT_IDS.filter((id) => isPlanned(id));
  if (planned.length === 0) {
    // The shell renders no planned element, so it must show no badge.
    await expect(shell.getByText("Planned", { exact: true })).toHaveCount(0);
    return;
  }
  const el = win.locator(`[data-element="${planned[0]}"]`);
  await expect(
    el.locator("[data-planned-badge]").getByText("Planned", { exact: true }),
  ).toBeVisible();
});

test("dropdown keeps the keyboard-active option visible", async ({ page }) => {
  await page.goto("/");
  const win = await chapterWindow(page, "capture");
  const before = await page.evaluate(() => window.scrollY);
  await win.getByRole("button", { name: /^Capture source/ }).focus();
  await page.keyboard.press("ArrowDown");
  const list = win.getByRole("listbox", { name: "Capture source" });
  await expect(list).toBeFocused();
  await page.keyboard.press("End");
  const last = list.getByRole("option").last();
  const l = (await list.boundingBox())!;
  const o = (await last.boundingBox())!;
  expect(o.y).toBeGreaterThanOrEqual(l.y - 1);
  expect(o.y + o.height).toBeLessThanOrEqual(l.y + l.height + 1);
  // The active option is visibly marked (accent outline).
  const activeId = await list.getAttribute("aria-activedescendant");
  expect(await last.getAttribute("id")).toBe(activeId);
  expect(await last.evaluate((e) => getComputedStyle(e).outlineStyle)).toBe(
    "solid",
  );
  expect(await page.evaluate(() => window.scrollY)).toBe(before);
});

test("dropdown near the window bottom opens upward inside the window", async ({
  page,
}) => {
  await page.goto("/");
  const win = await chapterWindow(page, "capture");
  // Simulate a control at the bottom of the window: move the capture bar
  // below the page area.
  await page.addStyleTag({
    content: "#capture .app-main { flex-direction: column-reverse; }",
  });
  await win.getByRole("button", { name: /^Capture source/ }).click();
  const list = win.getByRole("listbox", { name: "Capture source" });
  await expect(list).toHaveAttribute("data-placement", "top");
  const w = (await win.boundingBox())!;
  for (const box of [
    (await list.boundingBox())!,
    (await list.getByRole("option").first().boundingBox())!,
  ]) {
    expect(box.y).toBeGreaterThanOrEqual(w.y);
    expect(box.y + box.height).toBeLessThanOrEqual(w.y + w.height);
  }
});
