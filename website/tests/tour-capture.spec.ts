import { expect, test, type Page } from "@playwright/test";

test.use({ viewport: { width: 1440, height: 960 } });

/** Scrolls the Capture chapter into view, waits for hydration and the page. */
async function captureWindow(page: Page) {
  await page.goto("/");
  const section = page.locator("#capture");
  await section.scrollIntoViewIfNeeded();
  await expect(section.locator("astro-island").first()).not.toHaveAttribute(
    "ssr",
  );
  const win = section.locator("[data-app-window]");
  await expect(win.getByRole("heading", { name: "Audio Mixer" })).toBeVisible();
  return win;
}

const dbValue = async (el: ReturnType<Page["locator"]>) =>
  Number((await el.innerText()).replace(/[^\d.\-−]/g, "").replace("−", "-"));

test("source change updates card and top bar", async ({ page }) => {
  const win = await captureWindow(page);
  await win.getByRole("button", { name: /^Source to capture/ }).click();
  await win.getByRole("option", { name: /Neon Drift/ }).click();
  await expect(win.locator("[data-capture-source-card]")).toContainText(
    "Neon Drift",
  );
  await expect(
    win.getByRole("button", { name: /^Capture source Neon Drift/ }),
  ).toBeVisible();
});

test("estimate updates with replay length", async ({ page }) => {
  const win = await captureWindow(page);
  const est = win.locator("[data-estimate]");
  await expect(est).toContainText("≈ 150 MB / Clip");
  await win.getByRole("button", { name: /^Replay length/ }).click();
  await win.getByRole("option", { name: "120 sec" }).click();
  await expect(est).toContainText("≈ 300 MB / Clip");
  await expect(win.getByText("Estimate", { exact: true })).toBeVisible();
});

test("gain slider steps with ArrowRight", async ({ page }) => {
  const win = await captureWindow(page);
  const readout = win.locator('[data-gain-readout="mic"]');
  const before = await dbValue(readout);
  const slider = win.getByRole("slider", { name: "Microphone gain" });
  const step = Number(await slider.getAttribute("step"));
  await slider.focus();
  await page.keyboard.press("ArrowRight");
  expect(await dbValue(readout)).toBeCloseTo(before + step);
});

test("mute and solo toggle aria-pressed", async ({ page }) => {
  const win = await captureWindow(page);
  for (const name of ["Mute Microphone", "Solo Microphone"]) {
    const b = win.getByRole("button", { name });
    await expect(b).toHaveAttribute("aria-pressed", "false");
    await b.click();
    await expect(b).toHaveAttribute("aria-pressed", "true");
  }
});

test("recent clip play opens editor in same window", async ({ page }) => {
  const win = await captureWindow(page);
  await win
    .getByRole("button", { name: /^Play / })
    .first()
    .click();
  await expect(win).toHaveAttribute("data-page", "editor");
});

test("View All Clips opens library", async ({ page }) => {
  const win = await captureWindow(page);
  await win.getByRole("button", { name: /View All Clips/ }).click();
  await expect(win).toHaveAttribute("data-page", "library");
});

test("Add Audio Source adds a fourth row", async ({ page }) => {
  const win = await captureWindow(page);
  const rows = win.locator("[data-mixer-row]");
  await expect(rows).toHaveCount(3);
  await win.getByRole("button", { name: /Add Audio Source/ }).click();
  await expect(rows).toHaveCount(4);
});

test("favorite shows filled star in recent clips", async ({ page }) => {
  const win = await captureWindow(page);
  const rows = win.locator("[data-recent-clip]");
  // Initial data: second-newest clip is a favorite.
  await expect(rows.nth(1).locator("[data-favorite-star]")).toBeVisible();
  await expect(rows.nth(0).locator("[data-favorite-star]")).toHaveCount(0);
  await rows
    .nth(0)
    .getByRole("button", { name: /^More actions/ })
    .click();
  await win.getByRole("menuitem", { name: "Favorite" }).click();
  await expect(rows.nth(0).locator("[data-favorite-star]")).toBeVisible();
});

test.describe("reduced motion", () => {
  test("meters stay still", async ({ page }) => {
    await page.emulateMedia({ reducedMotion: "reduce" });
    const win = await captureWindow(page);
    const lit = () =>
      win
        .locator("[role=meter]")
        .evaluateAll((ms) =>
          ms.map((m) => m.querySelectorAll("[data-on]").length),
        );
    const a = await lit();
    await page.waitForTimeout(1000);
    expect(await lit()).toEqual(a);
  });
});
