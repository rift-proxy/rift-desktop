// Keyboard-only use: focus order, activation keys, modal focus trap and Escape.
// Complements axe (Storybook a11y addon), which cannot judge focus.
import { test, expect, type Page } from "@playwright/test";
async function screen(page: Page, story: string) {
  await page.goto(
    `/iframe.html?id=${encodeURIComponent("rift-экраны--" + story)}&viewMode=story`,
  );
  await expect(page.getByRole("navigation")).toBeVisible();
}
/** Focus is not on page content behind the modal (inside it, or left to the host/body). */
const notBehindModal = (page: Page) =>
  page.evaluate(() => {
    const active = document.activeElement;
    return !active || active === document.body || !!active.closest("dialog");
  });
const focusedInside = (page: Page, selector: string) =>
  page.evaluate((s) => !!document.activeElement?.closest(s), selector);

test("keyboard: Tab starts in navigation, Enter and Space switch pages", async ({
  page,
}) => {
  await screen(page, "disconnected");
  await page.keyboard.press("Tab");
  expect(await focusedInside(page, "nav")).toBe(true);
  const nav = page.getByRole("navigation");
  await nav.getByRole("button", { name: /Серверы/ }).focus();
  await page.keyboard.press("Enter");
  await expect(nav.getByRole("button", { name: /Серверы/ })).toHaveAttribute(
    "aria-current",
    "page",
  );
  await nav.getByRole("button", { name: /Журнал/ }).focus();
  await page.keyboard.press("Space");
  await expect(nav.getByRole("button", { name: /Журнал/ })).toHaveAttribute(
    "aria-current",
    "page",
  );
});

test("keyboard: switch toggles with Space", async ({ page }) => {
  await screen(page, "connected");
  const auto = page.getByRole("switch", { name: "Автовыбор сервера" });
  const before = await auto.getAttribute("aria-checked");
  await auto.focus();
  await page.keyboard.press("Space");
  await expect(auto).not.toHaveAttribute("aria-checked", before ?? "");
});

test("keyboard: dialog traps focus, Escape closes it and focus returns", async ({
  page,
}) => {
  await screen(page, "servers");
  const opener = page.getByRole("button", {
    name: "Добавить сервер",
    exact: true,
  });
  await opener.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("dialog")).toBeVisible();
  expect(await focusedInside(page, "dialog")).toBe(true);

  // A native modal makes the page behind it inert: Tab cycles through the dialog and may
  // step out to the browser/WebView host, never onto the page content behind it.
  const visited = new Set<string>();
  for (let i = 0; i < 8; i++) {
    await page.keyboard.press("Tab");
    expect(await notBehindModal(page), `Tab ${i + 1}`).toBe(true);
    visited.add(
      await page.evaluate(
        () => document.activeElement?.outerHTML.slice(0, 60) ?? "",
      ),
    );
  }
  expect(
    visited.size,
    "Tab reaches several controls in the dialog",
  ).toBeGreaterThan(2);
  await page.keyboard.press("Shift+Tab");
  expect(await notBehindModal(page)).toBe(true);
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toBeHidden();
  await expect(opener).toBeFocused();
});

test("keyboard: Escape does not dismiss the install dialog mid-install", async ({
  page,
}) => {
  // Standalone update story: play() drives it to the install-in-progress state.
  await page.goto(
    `/iframe.html?id=${encodeURIComponent("rift-обновление--installing")}&viewMode=story`,
  );
  await expect(page.getByRole("button", { name: "Отмена" })).toBeDisabled();
  await expect(
    page.getByRole("button", { name: "Закрыть диалог" }),
  ).toBeDisabled();
  // More presses than Chromium lets a `cancel` handler absorb (see the profile tests).
  for (let press = 1; press <= 4; press++) {
    await page.keyboard.press("Escape");
    await page.waitForTimeout(50);
    await expect(page.getByRole("dialog"), `Escape ${press}`).toBeVisible();
  }
  await expect(page.getByRole("button", { name: "Отмена" })).toBeDisabled();
});

test("keyboard: after a profile import the result stays and focus returns to Импортировать", async ({
  page,
}) => {
  // The story's play() applies a profile whose settings differ from the current ones, which
  // changes the state under the open Settings screen. The screen must not be rebuilt: the
  // result stays, and the control that opened the dialog (disabled while the file was
  // being picked, so the browser had already dropped focus) gets the focus back.
  await screen(page, "profile-imported");
  await expect(page.getByText("Профиль импортирован.")).toBeVisible();
  await expect(page.getByRole("dialog")).toBeHidden();
  await expect(
    page.getByRole("button", { name: "Импортировать" }),
  ).toBeFocused();
  await expect(page.getByLabel("Порт", { exact: true })).toHaveValue("3090");
});

for (const [story, operation, button] of [
  ["profile-exporting", "export", "Экспортировать"],
  ["profile-importing", "import", "Импортировать"],
] as const) {
  test(`keyboard: Escape does not dismiss the profile ${operation} dialog while it runs`, async ({
    page,
  }) => {
    // play() confirms the operation and leaves it unanswered: the dialog is busy.
    await screen(page, story);
    const dialog = page.getByRole("dialog");
    await expect(dialog.getByRole("button", { name: "Отмена" })).toBeDisabled();
    // Chromium honours a prevented Escape only twice without a click in between: the third
    // press closes a plain `cancel`-guarded dialog. Press more than that.
    for (let press = 1; press <= 4; press++) {
      await page.keyboard.press("Escape");
      await page.waitForTimeout(50);
      await expect(dialog, `Escape ${press}`).toBeVisible();
    }
    await expect(dialog.getByRole("button", { name: "Отмена" })).toBeDisabled();
    await expect(
      dialog.getByRole("button", { name: button, exact: true }),
    ).toBeDisabled();
  });
}

test("keyboard: journal export requires a privacy confirmation and Escape cancels", async ({
  page,
}) => {
  await screen(page, "journal");
  const opener = page.getByRole("button", { name: "Экспорт диагностики" });
  await opener.focus();
  await page.keyboard.press("Enter");
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText("Rift ничего не отправляет автоматически");
  await expect(dialog.getByRole("button", { name: "Отмена" })).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(dialog).not.toBeVisible();
  await expect(opener).toBeFocused();
});
