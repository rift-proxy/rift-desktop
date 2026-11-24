import { expect, test, type Page } from "@playwright/test";

async function openSettings(page: Page) {
  await page.goto(
    `/iframe.html?id=${encodeURIComponent("rift-экраны--settings")}&viewMode=story`,
  );
  await expect(
    page.getByRole("heading", { name: "Настройки", exact: true }),
  ).toBeVisible();
  await page.evaluate(() => document.fonts.ready);
}

test("top navigation stays visible without overlap when scrolled, and settings have no save button", async ({
  page,
}) => {
  await openSettings(page);
  await page.evaluate(() =>
    window.scrollTo(0, document.documentElement.scrollHeight),
  );
  await expect
    .poll(() => page.evaluate(() => window.scrollY))
    .toBeGreaterThan(0);
  const navigation = page.getByRole("navigation", {
    name: "Основная навигация",
  });
  const nav = (await navigation.boundingBox())!;
  const header = (await page.locator(".topbar").boundingBox())!;
  expect(nav.y).toBeGreaterThanOrEqual(0);
  expect(nav.y + nav.height).toBeLessThanOrEqual(page.viewportSize()!.height);
  expect(header.y).toBe(0);
  // Settings save themselves: no button, and nothing but the top bar is pinned.
  await expect(
    page.getByRole("button", { name: "Сохранить настройки" }),
  ).toHaveCount(0);
  expect(
    await page
      .locator("main *")
      .evaluateAll(
        (elements) =>
          elements.filter(
            (element) => getComputedStyle(element).position === "sticky",
          ).length,
      ),
  ).toBe(0);
  // Bounding boxes alone do not detect one sticky element painting over another.
  for (const label of ["Подключение", "Серверы", "Настройки", "Журнал"]) {
    const button = navigation.getByRole("button", { name: label, exact: true });
    const box = (await button.boundingBox())!;
    expect(
      await button.evaluate(
        (element, point) =>
          element.contains(document.elementFromPoint(point.x, point.y)),
        { x: box.x + box.width / 2, y: box.y + box.height / 2 },
      ),
      label,
    ).toBe(true);
  }
  await navigation.getByRole("button", { name: "Журнал", exact: true }).click();
  await expect(
    navigation.getByRole("button", { name: "Журнал", exact: true }),
  ).toHaveAttribute("aria-current", "page");
});

test("keyboard focus scrolled back into settings is not hidden under the top bar", async ({
  page,
}) => {
  await openSettings(page);
  const controls = page
    .locator("#settings-form")
    .locator("input, select, button");
  expect(await controls.count()).toBeGreaterThan(5);
  const last = controls.last();
  await last.focus();
  await expect(last).toBeFocused();
  await expect
    .poll(() => page.evaluate(() => window.scrollY))
    .toBeGreaterThan(0);
  // Reverse tabbing must reveal fields approached from below the visible viewport.
  let visited = 0;
  while (
    await page.evaluate(
      () => !!document.activeElement?.closest("#settings-form"),
    )
  ) {
    const active = page.locator(":focus");
    const box = (await active.boundingBox())!;
    const header = (await page.locator(".topbar").boundingBox())!;
    expect(
      box.y,
      (await active.getAttribute("name")) ?? "focused settings control",
    ).toBeGreaterThanOrEqual(header.y + header.height);
    expect(box.y + box.height).toBeLessThanOrEqual(page.viewportSize()!.height);
    expect(
      await active.evaluate((element) => {
        const box = element.getBoundingClientRect();
        return element.contains(
          document.elementFromPoint(
            box.x + box.width / 2,
            box.y + box.height / 2,
          ),
        );
      }),
    ).toBe(true);
    visited++;
    await page.keyboard.press("Shift+Tab");
  }
  expect(visited).toBeGreaterThan(5);
});

test("interface text cannot be selected, fields keep their text selectable", async ({
  page,
}) => {
  await page.goto(
    `/iframe.html?id=${encodeURIComponent("rift-экраны--servers")}&viewMode=story`,
  );
  await expect(
    page.getByRole("button", { name: "Warsaw 03", exact: true }),
  ).toBeVisible();
  expect(
    await page.evaluate(() => getComputedStyle(document.body).userSelect),
  ).toBe("none");
  await page.getByRole("heading", { name: "Серверы" }).dblclick();
  await page.keyboard.press("Control+A");
  expect(
    await page.evaluate(() => window.getSelection()?.toString().trim()),
  ).toBe("");
  await page.goto(
    `/iframe.html?id=${encodeURIComponent("rift-экраны--settings")}&viewMode=story`,
  );
  const port = page.getByLabel("Порт", { exact: true });
  await expect(port).toBeVisible();
  expect(await port.evaluate((el) => getComputedStyle(el).userSelect)).toBe(
    "text",
  );
});

test("dragging across a server row does not switch the connection", async ({
  page,
}) => {
  await page.goto(
    `/iframe.html?id=${encodeURIComponent("rift-экраны--servers")}&viewMode=story`,
  );
  const selected = page
    .getByRole("button", { name: "Frankfurt 01", exact: true })
    .locator("xpath=ancestor::tr");
  const target = page
    .getByRole("button", { name: "Warsaw 03", exact: true })
    .locator("xpath=ancestor::tr");
  await expect(selected).toHaveClass(/selected/);
  const latency = target.getByText("74 мс", { exact: true });
  await latency.scrollIntoViewIfNeeded();
  const box = (await latency.boundingBox())!;
  const y = box.y + box.height / 2;
  // Press in the latency cell and release far to its left, in the same row.
  await page.mouse.move(box.x + box.width / 2, y);
  await page.mouse.down();
  await page.mouse.move(box.x - 120, y, { steps: 12 });
  await page.mouse.up();
  await expect(selected).toHaveClass(/selected/);
  await expect(target).not.toHaveClass(/selected/);
  // The next ordinary click still selects.
  await latency.click();
  await expect(target).toHaveClass(/selected/);
});
