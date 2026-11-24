import { test, expect, type Page } from "@playwright/test";

async function open(page: Page, story: string) {
  await page.goto(
    `/iframe.html?id=${encodeURIComponent("rift-экраны--" + story)}&viewMode=story`,
  );
  await expect(page.getByRole("navigation")).toBeVisible();
  await page.evaluate(() => document.fonts.ready);
}

test("every page starts at the same left edge", async ({ page }) => {
  const edges: Record<string, number> = {};
  for (const story of [
    "disconnected",
    "servers",
    "connections",
    "routing",
    "settings",
    "journal",
  ]) {
    await open(page, story);
    // The connection page has no title: its first heading is the state.
    const box = await page
      .locator("main")
      .getByRole("heading")
      .first()
      .boundingBox();
    edges[story] = box!.x;
  }
  expect(new Set(Object.values(edges)).size, JSON.stringify(edges)).toBe(1);
});

test("a setting's explanation sits 4px under its switch in 11px text", async ({
  page,
}) => {
  await open(page, "settings");
  const hints = page.locator(".setting-hint");
  await expect(hints.first()).toBeVisible();
  for (const hint of await hints.all()) {
    const label = hint.locator("xpath=preceding-sibling::*[1]");
    const [h, l] = [await hint.boundingBox(), await label.boundingBox()];
    expect(h!.y - (l!.y + l!.height)).toBeCloseTo(4, 0);
    await expect(hint).toHaveCSS("font-size", "11px");
  }
});

test("the update banner floats above the page and moves nothing", async ({
  page,
}) => {
  await open(page, "update-available");
  const title = page.locator("main").getByRole("heading").first();
  const before = await title.boundingBox();
  const banner = page.locator(".notification");
  await expect(banner).toBeVisible();
  expect(await banner.evaluate((el) => getComputedStyle(el).position)).toBe(
    "fixed",
  );
  expect(await title.boundingBox()).toEqual(before);
  // The banner stays inside the window.
  const box = (await banner.boundingBox())!;
  const view = page.viewportSize()!;
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.y + box.height).toBeLessThanOrEqual(view.height);
});

test("a progress line does not move the server table", async ({ page }) => {
  await open(page, "refreshing");
  const table = page.getByRole("table");
  await expect(page.getByText("Обновление подписок…")).toBeVisible();
  const busy = await table.boundingBox();
  await open(page, "servers");
  expect(await table.boundingBox()).toEqual(busy);
});
