import { expect, test } from "@playwright/test";

const widths = [375, 430, 768, 1024, 1280, 1440, 1920];

for (const width of widths) {
  test(`W2 layout and interactions at ${width}px`, async ({
    browser,
  }, info) => {
    const context = await browser.newContext({
      viewport: { width, height: 960 },
      hasTouch: width <= 1024,
    });
    const page = await context.newPage();
    const errors: string[] = [];
    page.on("pageerror", (error) => errors.push(error.message));
    page.on("console", (message) => {
      if (message.type() === "error") errors.push(message.text());
    });
    page.on("response", (response) => {
      if (response.status() >= 400)
        errors.push(`${response.status()} ${response.url()}`);
    });
    await page.goto("/");
    await expect(page.getByRole("main")).toBeVisible();
    const menu = page.locator("[data-menu-button]");
    if (width < 768) {
      await menu.tap();
      await expect(menu).toHaveAttribute("aria-expanded", "true");
      await page.keyboard.press("Escape");
      await expect(menu).toBeFocused();
      await menu.tap();
      await page
        .getByRole("navigation", { name: "Primary mobile" })
        .getByRole("link", { name: "Showcase", exact: true })
        .tap();
      await expect(menu).toHaveAttribute("aria-expanded", "false");
    } else {
      await page
        .getByRole("navigation", { name: "Primary", exact: true })
        .getByRole("link", { name: "Showcase", exact: true })
        .click();
    }

    const replay = page.getByRole("group", {
      name: "Instant replay demo",
      exact: true,
    });
    await replay.scrollIntoViewIfNeeded();
    const save = replay.getByRole("button", {
      name: "Press Ctrl+Shift+F10",
      exact: true,
    });
    await expect(save).toBeEnabled();
    await replay.getByLabel("30 s", { exact: true }).check();
    await replay
      .getByRole("combobox", { name: "Demo hotkey", exact: true })
      .selectOption("F9");
    const customSave = replay.getByRole("button", {
      name: "Press Ctrl+Shift+F9",
      exact: true,
    });
    await customSave.focus();
    await page.keyboard.press("Control+Shift+F10");
    await expect(replay.getByRole("listitem")).toHaveCount(1);
    await expect(
      replay.getByText("No clips yet.", { exact: false }),
    ).toBeVisible();
    await page.keyboard.press("Control+Shift+F9");
    await expect(replay.getByRole("status")).toHaveText(
      "Clip saved, last 30 seconds. Demo clip 1.",
    );
    for (let i = 2; i <= 4; i++) {
      await customSave.click();
      await expect(replay.getByRole("status")).toHaveText(
        `Clip saved, last 30 seconds. Demo clip ${i}.`,
      );
    }
    await expect(replay.getByRole("listitem")).toHaveCount(3);

    const mixer = page.locator("#audio-mixer");
    await mixer.scrollIntoViewIfNeeded();
    await expect(mixer.locator("astro-island")).not.toHaveAttribute("ssr");
    for (const source of [
      "Game audio",
      "Discord",
      "Desktop audio",
      "Microphone",
    ]) {
      const gain = mixer.getByRole("slider", {
        name: `${source} gain`,
        exact: true,
      });
      if (source === "Microphone") {
        await expect(gain).toBeDisabled();
        await mixer
          .getByRole("button", { name: "Microphone Off", exact: true })
          .click();
      }
      await expect(gain).toBeEnabled();
      await gain.focus();
      await page.keyboard.press("Home");
      await page.keyboard.press("ArrowRight");
      await expect(gain).toHaveValue("50");
      await mixer
        .getByRole("button", { name: `${source} On`, exact: true })
        .click();
      await expect(gain).toBeDisabled();
      await mixer
        .getByRole("button", { name: `${source} Off`, exact: true })
        .click();
      await expect(gain).toHaveValue("50");
      const details = mixer.getByRole("button", {
        name: `${source} details`,
        exact: true,
      });
      await details.click();
      await expect(details).toHaveAttribute("aria-expanded", "true");
    }
    const separate = mixer.getByRole("button").filter({ hasText: "Separate" });
    await separate.click();
    await expect(separate).toHaveAttribute("aria-pressed", "false");

    const editor = page.locator("#clip-editor");
    await editor.scrollIntoViewIfNeeded();
    await expect(editor.locator("astro-island")).not.toHaveAttribute("ssr");
    for (const source of [
      "Game audio",
      "Discord",
      "Desktop audio",
      "Microphone",
    ]) {
      const mute = editor.getByRole("button", {
        name: `Mute ${source} (planned)`,
        exact: true,
      });
      await expect(mute).toBeVisible();
      if (width <= 1024) {
        const box = await mute.boundingBox();
        expect(box!.width).toBeGreaterThanOrEqual(44);
        expect(box!.height).toBeGreaterThanOrEqual(44);
        await mute.tap();
      } else await mute.click();
      await expect(mute).toHaveAttribute("aria-pressed", "true");
      await mute.click();
      await expect(mute).toHaveAttribute("aria-pressed", "false");
    }
    const head = editor.getByRole("slider", { name: "Playhead", exact: true });
    await head.focus();
    await page.keyboard.press("Home");
    await page.keyboard.press("ArrowRight");
    await expect(head).toHaveAttribute("aria-valuenow", "1");
    const start = editor.getByRole("slider", {
      name: "Trim start",
      exact: true,
    });
    await start.focus();
    await page.keyboard.press("End");
    await expect(start).toHaveAttribute("aria-valuenow", "205");
    await page.keyboard.press("Home");
    await expect(start).toHaveAttribute("aria-valuenow", "0");
    const end = editor.getByRole("slider", { name: "Trim end", exact: true });
    await end.focus();
    await page.keyboard.press("Home");
    await expect(end).toHaveAttribute("aria-valuenow", "10");
    await page.keyboard.press("End");
    await expect(end).toHaveAttribute("aria-valuenow", "300");
    await end.scrollIntoViewIfNeeded();
    const box = await end.boundingBox();
    await page.mouse.move(box!.x + box!.width / 2, box!.y + box!.height / 2);
    await page.mouse.down();
    await page.mouse.move(box!.x - 30, box!.y + box!.height / 2, { steps: 5 });
    await page.mouse.up();
    expect(Number(await end.getAttribute("aria-valuenow"))).toBeLessThan(300);
    if (width <= 1024) {
      await end.focus();
      await page.keyboard.press("End");
      const touchBox = await end.boundingBox();
      const cdp = await context.newCDPSession(page);
      const x = touchBox!.x + touchBox!.width / 2;
      const y = touchBox!.y + 20;
      await cdp.send("Input.dispatchTouchEvent", {
        type: "touchStart",
        touchPoints: [{ x, y }],
      });
      await cdp.send("Input.dispatchTouchEvent", {
        type: "touchMove",
        touchPoints: [{ x: x - 30, y }],
      });
      await cdp.send("Input.dispatchTouchEvent", {
        type: "touchEnd",
        touchPoints: [],
      });
      expect(Number(await end.getAttribute("aria-valuenow"))).toBeLessThan(300);
      await cdp.detach();
    }
    await editor
      .getByRole("button", { name: "Play preview", exact: true })
      .click();
    await expect(
      editor.getByRole("button", { name: "Pause preview", exact: true }),
    ).toBeVisible();
    await editor
      .getByRole("button", { name: "Pause preview", exact: true })
      .click();
    await editor
      .getByRole("button", { name: "Export trimmed clip", exact: true })
      .click();
    await expect(
      editor.getByText("Simulated: no file was created.", { exact: false }),
    ).toBeVisible();

    for (const section of await page.locator("main section").all()) {
      await section.scrollIntoViewIfNeeded();
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth,
        ),
      ).toBe(true);
    }
    await expect(
      page.getByText("Coming soon", { exact: true }).first(),
    ).toBeVisible();
    await page.evaluate(() => window.scrollTo({ top: 0, behavior: "instant" }));
    await page.screenshot({
      path: info.outputPath(`homepage-${width}.png`),
      fullPage: true,
      // Offscreen scroll-timeline content must be visible in the full-page artifact.
      style:
        ".reveal { animation: none !important; opacity: 1 !important; transform: none !important; }",
    });
    await editor.scrollIntoViewIfNeeded();
    await editor
      .locator("astro-island > div")
      .screenshot({ path: info.outputPath(`editor-${width}.png`) });
    expect(errors).toEqual([]);
    await context.close();
  });
}

test("static content and mobile navigation remain usable without JavaScript", async ({
  browser,
}) => {
  const context = await browser.newContext({
    javaScriptEnabled: false,
    viewport: { width: 375, height: 960 },
  });
  const page = await context.newPage();
  await page.goto("/");
  await expect(
    page.getByRole("navigation", { name: "Primary mobile" }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "Make every clip yours.", exact: true }),
  ).toBeVisible();
  for (const link of await page.locator('a[href^="#"]').all()) {
    const href = await link.getAttribute("href");
    await expect(page.locator(href!)).toHaveCount(1);
  }
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  await context.close();
});

test("reduced motion keeps replay and editor functional", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto("/");
  const replay = page.getByRole("group", {
    name: "Instant replay demo",
    exact: true,
  });
  await replay.scrollIntoViewIfNeeded();
  await replay
    .getByRole("button", { name: "Press Ctrl+Shift+F10", exact: true })
    .click();
  await expect(replay.getByRole("status")).toHaveText(
    "Clip saved, last 1 minute. Demo clip 1.",
  );
  const editor = page.locator("#clip-editor");
  await editor.scrollIntoViewIfNeeded();
  await editor
    .getByRole("button", { name: "Jump to trim end", exact: true })
    .click();
  await expect(
    editor.getByRole("slider", { name: "Playhead", exact: true }),
  ).toHaveAttribute("aria-valuenow", "215");
  expect(await page.locator(".replay-sheen").count()).toBe(0);
});
