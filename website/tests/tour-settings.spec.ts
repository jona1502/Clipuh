import { expect, test, type Page } from "@playwright/test";

test.use({ viewport: { width: 1440, height: 960 } });

/** Scrolls the Settings chapter into view and waits for the page to render. */
async function settingsWindow(page: Page) {
  await page.goto("/");
  const section = page.locator("#settings");
  await section.scrollIntoViewIfNeeded();
  await expect(section.locator("astro-island").first()).not.toHaveAttribute(
    "ssr",
  );
  const win = section.locator("[data-app-window]");
  await expect(win.locator("[data-settings-panel='video']")).toBeVisible();
  return win;
}

/** Hydrates every chapter window (islands hydrate when visible), then returns to Settings. */
async function hydrateAll(page: Page) {
  for (const id of ["capture", "library", "editor", "settings"]) {
    const section = page.locator(`#${id}`);
    await section.scrollIntoViewIfNeeded();
    await expect(section.locator("astro-island").first()).not.toHaveAttribute(
      "ssr",
    );
  }
}
const hotkeyRow = (win: ReturnType<Page["locator"]>, name: string) =>
  win.locator("[data-hotkey-row]").filter({ hasText: name });

test("category nav", async ({ page }) => {
  const win = await settingsWindow(page);
  const before = await page.evaluate(() => window.scrollY);
  const item = win
    .getByRole("navigation", { name: "Settings categories" })
    .getByRole("button", { name: "Hotkeys" });
  await item.click();
  await expect(item).toHaveAttribute("aria-current", "true");
  const panel = win.locator("[data-settings-panel='hotkeys']");
  await expect
    .poll(async () => {
      const p = await panel.boundingBox();
      const c = await win.locator(".app-page").boundingBox();
      return !!p && !!c && p.y >= c.y - 2 && p.y + 20 <= c.y + c.height;
    })
    .toBe(true);
  expect(await page.evaluate(() => window.scrollY)).toBe(before);
});

test("accent recolors all windows", async ({ page }) => {
  const win = await settingsWindow(page);
  await hydrateAll(page);
  await page.locator("#settings").scrollIntoViewIfNeeded();
  const btn = page.locator("[data-site-header] a.ml-3");
  const color = () => btn.evaluate((e) => getComputedStyle(e).backgroundColor);
  const before = await color();
  await win.getByRole("radio", { name: "Green" }).click();
  const all = page.locator("[data-app-window]");
  const n = await all.count();
  expect(n).toBeGreaterThan(1);
  for (let i = 0; i < n; i++)
    await expect(all.nth(i)).toHaveAttribute("data-accent", "green");
  expect(await color()).toBe(before);
});

test("theme light", async ({ page }) => {
  const win = await settingsWindow(page);
  await hydrateAll(page);
  await page.locator("#settings").scrollIntoViewIfNeeded();
  const bg = () =>
    page.evaluate(() => getComputedStyle(document.body).backgroundColor);
  const before = await bg();
  await win.getByRole("radio", { name: "Light" }).click();
  const all = page.locator("[data-app-window]");
  const n = await all.count();
  for (let i = 0; i < n; i++)
    await expect(all.nth(i)).toHaveAttribute("data-theme", "light");
  expect(await bg()).toBe(before);
});

test("hotkey record", async ({ page }) => {
  const win = await settingsWindow(page);
  const row = hotkeyRow(win, "Start / Stop Recording");
  await expect(row).toContainText("Alt + R");
  await row.getByRole("button", { name: /^Change/ }).click();
  await page.keyboard.press("Alt+K");
  await expect(row).toContainText("Alt + K");

  // Conflict with Save Clip (Alt + S): error shown, previous combo kept.
  await row.getByRole("button", { name: /^Change/ }).click();
  await page.keyboard.press("Alt+S");
  await expect(win.getByRole("alert")).toContainText("already used");
  await expect(row).toContainText("Alt + K");

  // Escape cancels and clears the error.
  await page.keyboard.press("Escape");
  await expect(win.getByRole("alert")).toHaveCount(0);
  await expect(row).toContainText("Alt + K");
});

test("hotkey conflict keeps combo", async ({ page }) => {
  const win = await settingsWindow(page);
  const row = hotkeyRow(win, "Start / Stop Recording");
  await row.getByRole("button", { name: /^Change/ }).click();
  await page.keyboard.press("Alt+S");
  await expect(win.getByRole("alert")).toContainText("already used");
  await expect(row).toContainText("Alt + R");
});

test("resolution sync", async ({ page }) => {
  const win = await settingsWindow(page);
  await hydrateAll(page);
  await page.locator("#settings").scrollIntoViewIfNeeded();
  await win
    .locator("[data-settings-panel=video]")
    .getByRole("button", { name: /^Resolution/ })
    .click();
  await win.getByRole("option", { name: "1440p" }).click();
  const top = page.locator("#capture [data-app-window]");
  await expect(
    top
      .locator("[data-app-shell-chrome=topbar]")
      .getByRole("button", { name: /^Resolution/ }),
  ).toContainText("1440p");
});

test("checkbox", async ({ page }) => {
  const win = await settingsWindow(page);
  const box = win.getByRole("checkbox", { name: "Separate audio tracks" });
  await expect(box).toBeChecked();
  await box.click();
  await expect(box).not.toBeChecked();
  await box.click();
  await expect(box).toBeChecked();
});
