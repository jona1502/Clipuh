import { expect, test, type Page } from "@playwright/test";
import { tourChapters } from "../src/data/features";

/*
 * Homepage as a whole: every QA width, header navigation to the four
 * chapters, the stacked phone layout and the no-JavaScript fallback.
 */

const widths = [375, 430, 768, 1024, 1280, 1440, 1920];
const chapters = ["capture", "library", "editor", "settings"] as const;
const navLabel = {
  capture: "Capture",
  library: "Library",
  editor: "Editor",
  settings: "Settings",
} as const;

/** Collects console errors, page errors and 4xx/5xx responses. */
function trackErrors(page: Page) {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  page.on("console", (m) => {
    if (m.type() === "error") errors.push(m.text());
  });
  page.on("response", (r) => {
    if (r.status() >= 400) errors.push(`${r.status()} ${r.url()}`);
  });
  return errors;
}

const noHorizontalScroll = (page: Page) =>
  page.evaluate(() => document.documentElement.scrollWidth <= innerWidth);

/** Top of the section relative to the viewport. */
const sectionTop = (page: Page, id: string) =>
  page.locator(`#${id}`).evaluate((e) => e.getBoundingClientRect().top);

for (const width of widths) {
  test(`tour page at ${width}px`, async ({ browser }) => {
    const context = await browser.newContext({
      viewport: { width, height: 900 },
      hasTouch: width <= 1024,
    });
    const page = await context.newPage();
    const errors = trackErrors(page);
    await page.goto("/");
    await expect(page.getByRole("main")).toBeVisible();
    expect(await noHorizontalScroll(page)).toBe(true);

    // Header nav reaches every chapter (mobile menu below 768px).
    for (const id of chapters) {
      if (width < 768) {
        const menu = page.locator("[data-menu-button]");
        await menu.click();
        await expect(menu).toHaveAttribute("aria-expanded", "true");
        await page
          .getByRole("navigation", { name: "Primary mobile" })
          .getByRole("link", { name: navLabel[id], exact: true })
          .click();
        await expect(menu).toHaveAttribute("aria-expanded", "false");
      } else {
        await page
          .getByRole("navigation", { name: "Primary", exact: true })
          .getByRole("link", { name: navLabel[id], exact: true })
          .click();
      }
      await expect(page).toHaveURL(new RegExp(`#${id}$`));
      // Smooth scroll: the chapter settles just below the sticky header.
      await expect
        .poll(() => sectionTop(page, id), { timeout: 5000 })
        .toBeLessThan(140);
      expect(await sectionTop(page, id)).toBeGreaterThan(-4);
    }

    // Every chapter heading is visible once scrolled to.
    for (const id of chapters) {
      const heading = page.locator(`#${id}-title`);
      await heading.scrollIntoViewIfNeeded();
      await expect(heading).toBeVisible();
      await expect(heading).toHaveText(tourChapters[id].title);
      await expect(
        page.locator(`#${id}`).getByText("Interactive demo · sample data"),
      ).toBeVisible();
    }

    if (width === 375) {
      // Stacked layout: the sidebar is a tab row above the page.
      const capture = page.locator("#capture [data-app-window]");
      await page.locator("#capture").scrollIntoViewIfNeeded();
      await expect(capture.locator(".app-sidebar-mobile")).toBeVisible();
      await expect(capture.locator(".app-sidebar-desktop")).toBeHidden();
      const tabs = capture.locator(".app-sidebar-mobile");
      const page_ = capture.locator(".app-page");
      expect((await tabs.boundingBox())!.y).toBeLessThan(
        (await page_.boundingBox())!.y,
      );

      // The Editor timeline scrolls inside the window, not the page.
      const editor = page.locator("#editor");
      await editor.scrollIntoViewIfNeeded();
      await expect(editor.locator("astro-island").first()).not.toHaveAttribute(
        "ssr",
      );
      const lanes = editor.locator("[data-timeline-lanes]");
      await expect(lanes).toBeVisible();
      const scroll = await lanes.evaluate((e) => {
        const before = e.scrollWidth > e.clientWidth;
        e.scrollLeft = 40;
        return { before, left: e.scrollLeft };
      });
      expect(scroll.before).toBe(true);
      expect(scroll.left).toBeGreaterThan(0);
      const win = (await editor.locator("[data-app-window]").boundingBox())!;
      const box = (await lanes.boundingBox())!;
      expect(box.x).toBeGreaterThanOrEqual(win.x - 1);
      expect(box.x + box.width).toBeLessThanOrEqual(win.x + win.width + 1);
    }

    expect(await noHorizontalScroll(page)).toBe(true);
    expect(errors).toEqual([]);
    await context.close();
  });
}

test("chapter headings and copy are visible without JavaScript", async ({
  browser,
}) => {
  const context = await browser.newContext({
    javaScriptEnabled: false,
    viewport: { width: 375, height: 900 },
  });
  const page = await context.newPage();
  await page.goto("/");
  for (const id of chapters) {
    const copy = tourChapters[id];
    const section = page.locator(`#${id}`);
    await expect(section.locator(`#${id}-title`)).toHaveText(copy.title);
    await expect(section.locator(`#${id}-title`)).toBeVisible();
    await expect(section.getByText(copy.description)).toBeVisible();
    for (const point of copy.points)
      await expect(
        section
          .locator("[data-chapter-points]")
          .getByText(point.title, { exact: true }),
      ).toBeVisible();
  }
  // Without JS the mobile nav is a static link list that reaches each chapter.
  const nav = page.getByRole("navigation", { name: "Primary mobile" });
  await expect(nav).toBeVisible();
  for (const id of chapters)
    await expect(
      nav.getByRole("link", { name: navLabel[id], exact: true }),
    ).toHaveAttribute("href", `#${id}`);
  // Every in-page link has a target.
  for (const link of await page.locator('a[href^="#"]').all()) {
    const href = await link.getAttribute("href");
    await expect(page.locator(href!)).toHaveCount(1);
  }
  expect(await noHorizontalScroll(page)).toBe(true);
  await context.close();
});
