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
