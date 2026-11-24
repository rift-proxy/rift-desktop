import { test, expect, type Page } from "@playwright/test";
import { nextVersion } from "../src/testing/mock";
async function screen(page: Page, story: string) {
  await page.goto(
    `/iframe.html?id=${encodeURIComponent("rift-экраны--" + story)}&viewMode=story`,
  );
  await expect(page.getByRole("navigation")).toBeVisible();
  await page.evaluate(() => document.fonts.ready);
}
// The protocol (`vless reality`) and the status chips outside a failure reason are one-word-wide
// ovals, not paragraphs: only the chip of `td.status-reason` may wrap. Counts the lines of each
// chip's text, so a chip squeezed by the table layout is caught even when its height looks fine.
async function tableChips(page: Page) {
  return page.evaluate(() => {
    const chips = [
      ...document.querySelectorAll<HTMLElement>(
        "td:not(.status-reason) .badge",
      ),
    ];
    const lines = (chip: HTMLElement) => {
      const range = document.createRange();
      const text = [...chip.childNodes].find(
        (n) => n.nodeType === Node.TEXT_NODE,
      );
      if (text) range.selectNodeContents(text);
      return range.getClientRects().length;
    };
    return {
      total: chips.length,
      wrapped: chips
        .filter((chip) => lines(chip) !== 1)
        .map((chip) => `${chip.textContent} (${lines(chip)} lines)`),
    };
  });
}
const dialogProfileStories = new Set([
  "profile-export-warning",
  "profile-import-preview",
  "profile-importing",
  "profile-export-failed",
  "dark-profile-import-failed",
  "confirm-remove-subscription",
]);
const profileStoryReady: Record<string, (page: Page) => Promise<unknown>> = {
  "profile-importing": (page) =>
    expect(
      page.getByRole("dialog").getByRole("button", { name: "Отмена" }),
    ).toBeDisabled(),
  "profile-imported": async (page) => {
    await expect(page.getByText("Профиль импортирован.")).toBeVisible();
    await expect(page.getByLabel("Порт", { exact: true })).toHaveValue("3090");
  },
  "dark-profile-exported": (page) =>
    expect(page.getByText("Профиль экспортирован.")).toBeVisible(),
  "profile-export-failed": (page) =>
    expect(
      page.getByRole("dialog").getByRole("alert").filter({
        hasText: "Не удалось сохранить профиль",
      }),
    ).toBeVisible(),
  "dark-profile-import-failed": (page) =>
    expect(
      page.getByRole("dialog").getByRole("alert").filter({
        hasText: "текущие данные сохранены",
      }),
    ).toBeVisible(),
  starting: (page) =>
    expect(page.getByText("Подключение к Rift…")).toBeVisible(),
  "confirm-remove-subscription": (page) =>
    expect(
      page.getByRole("heading", { name: "Удалить «Основная»?" }),
    ).toBeVisible(),
  "tun-mode": (page) =>
    expect(page.getByText("Выключен в режиме TUN")).toBeVisible(),
  "server-details": (page) =>
    expect(page.getByRole("columnheader", { name: "Протокол" })).toBeVisible(),
  "dark-server-details": (page) =>
    expect(page.getByRole("columnheader", { name: "Протокол" })).toBeVisible(),
  "servers-sorted": (page) =>
    expect(page.getByRole("columnheader", { name: "Протокол" })).toBeVisible(),
  "servers-sorted-hidden": (page) =>
    expect(page.getByText("Порядок: Источник, по убыванию")).toBeVisible(),
  "dark-servers-sorted-back": (page) =>
    expect(page.getByRole("columnheader", { name: "Протокол" })).toBeVisible(),
  "protocol-chips": (page) =>
    expect(page.getByRole("columnheader", { name: "Протокол" })).toBeVisible(),
  "dark-protocol-chips": (page) =>
    expect(page.getByRole("columnheader", { name: "Протокол" })).toBeVisible(),
  "connection-details": (page) =>
    expect(
      page.getByRole("table", { name: "Последние замеры сервера" }),
    ).toBeVisible(),
  "dark-connection-details": (page) =>
    expect(
      page.getByRole("table", { name: "Последние замеры сервера" }),
    ).toBeVisible(),
  connections: (page) => expect(page.getByRole("table")).toBeVisible(),
  "dark-connections": (page) => expect(page.getByRole("table")).toBeVisible(),
  "connection-speed": (page) =>
    expect(page.getByText("4,6 МБ/с")).toBeVisible(),
  "connections-not-connected": (page) =>
    expect(
      page.getByRole("heading", { name: "Нет подключения" }),
    ).toBeVisible(),
  "journal-history": (page) =>
    expect(
      page.getByRole("table", { name: "Последние замеры сервера" }),
    ).toBeVisible(),
  "profile-inspect-failed": (page) =>
    expect(
      page
        .getByRole("alert")
        .filter({ hasText: "Некорректный формат профиля" }),
    ).toBeVisible(),
};
for (const story of [
  "connected",
  "disconnected",
  "connecting",
  "error",
  "connection-lost",
  "connection-speed",
  "dark",
  "dark-servers",
  "dark-settings",
  "dark-journal",
  "manual",
  "servers",
  "subscription-no-servers",
  "subscription-failed-selected",
  "server-status-tip",
  "connections-sorted",
  "servers-kept-out",
  "dark-servers-kept-out",
  "server-menu",
  "server-details",
  "dark-server-details",
  "servers-sorted",
  "servers-sorted-hidden",
  "dark-servers-sorted-back",
  "connection-details",
  "dark-connection-details",
  "no-servers",
  "no-subscriptions",
  "subscription-error",
  "failed-checks",
  "dark-failed-checks",
  "protocol-chips",
  "dark-protocol-chips",
  "refreshing",
  "dark-refreshing",
  "checking",
  "settings",
  "profile-export-warning",
  "profile-import-preview",
  "profile-importing",
  "profile-imported",
  "dark-profile-exported",
  "profile-export-failed",
  "dark-profile-import-failed",
  "profile-inspect-failed",
  "journal",
  "journal-by-source",
  "journal-history",
  "connections",
  "dark-connections",
  "connections-not-connected",
  "routing",
  "dark-routing",
  "routing-edited",
  "routing-databases",
  "journal-all-sources",
  "add-server-empty",
  "add-server-parsed",
  "add-server-invalid",
  "add-subscription",
  "subscription-usage",
  "dark-subscription-usage",
  "edit-subscription-options",
  "edit-subscription-routes-off",
  "welcome",
  "welcome-recognised",
  "dark-welcome",
  "pasted-servers",
  "pasted-subscription",
  "starting",
  "confirm-remove-subscription",
  "tun-mode",
  "error-toast",
  "notice-toast",
  "dark-error-toasts",
]) {
  test(`visual ${story}`, async ({ page }) => {
    await screen(page, story);
    // Errors are toasts: wait for them, they arrive as live events after the first paint.
    if (
      story.endsWith("error-toast") ||
      story.endsWith("error-toasts") ||
      story === "notice-toast"
    )
      await expect(page.locator(".toast").first()).toBeVisible();
    const profileReady = profileStoryReady[story];
    if (profileReady) await profileReady(page);
    // The long failure reason of a check is the point of these stories.
    if (story.endsWith("failed-checks"))
      await expect(
        page.getByText("Проверка через сервер заняла слишком много времени"),
      ).toBeVisible();
    // Eight chips: protocol and status of four rows, all of them multi-word or unchecked.
    if (story.endsWith("protocol-chips"))
      await expect(
        page.locator("td .badge").filter({ hasText: "Не проверен" }),
      ).toHaveCount(2);
    const chips = await tableChips(page);
    expect(chips.wrapped).toEqual([]);
    if (story.endsWith("protocol-chips")) expect(chips.total).toBe(8);
    if (story.endsWith("refreshing")) {
      await expect(page.getByText("Обновление подписок…")).toBeVisible();
      await expect(
        page.getByRole("button", { name: "Обновить все" }),
      ).toBeDisabled();
      await expect(
        page.getByRole("button", { name: "Обновить Основная" }),
      ).toBeDisabled();
    }
    // Each profile story ends in a specific state; wait for that state, not just the page.
    if (story.startsWith("add-") || dialogProfileStories.has(story)) {
      await expect(page.getByRole("dialog")).toBeVisible();
      if (story === "add-server-parsed")
        await expect(
          page.getByRole("heading", { name: "Новый сервер" }),
        ).toBeVisible();
      if (story === "add-server-invalid")
        await expect(page.getByRole("dialog").getByRole("alert")).toBeVisible();
    }
    // Stories that act after the first paint (a paste, typing): wait for what they end with.
    if (story === "routing-edited")
      await expect(page.getByText(/Последним стоит Block/)).toBeVisible();
    if (story === "journal-by-source")
      await expect(
        page.getByRole("button", { name: "Проверки серверов" }),
      ).toHaveAttribute("aria-pressed", "true");
    if (story === "pasted-servers")
      await expect(page.getByText("Найдено серверов: 8")).toBeVisible();
    // The address is read from the core; the field takes the focus when it arrives.
    if (story.startsWith("edit-subscription"))
      await expect(
        page.getByRole("dialog").getByLabel("Ссылка подписки"),
      ).toBeFocused();
    if (story === "pasted-subscription")
      await expect(
        page.getByRole("dialog").getByLabel("Ссылка подписки"),
      ).toHaveValue("https://panel.example/sub?token=abc");
    if (story.endsWith("welcome"))
      await expect(page.getByRole("dialog")).toBeVisible();
    if (story === "welcome-recognised")
      await expect(page.getByText("Подписка: panel.example")).toBeVisible();
    await expect(page).toHaveScreenshot(`${story}.png`, {
      fullPage: true,
      animations: "disabled",
    });
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
  });
}
// Standalone update banner/dialog stories, not part of the full-app screen loop.
// Each story's own play function drives it to its end state; wait for that state specifically
// before screenshotting, not just the banner (present from the start in every one of them).
const updateStoryReady: Record<string, (page: Page) => Promise<unknown>> = {
  banner: (page) => expect(page.getByText(/Доступна версия/)).toBeVisible(),
  "confirm-install": (page) =>
    expect(
      page.getByRole("heading", { name: `Установить версию ${nextVersion}?` }),
    ).toBeVisible(),
  installing: (page) =>
    expect(page.getByRole("button", { name: "Отмена" })).toBeDisabled(),
  "install-failed": (page) =>
    expect(page.getByRole("alert")).toHaveText("Сеть недоступна"),
};
for (const story of Object.keys(updateStoryReady).flatMap((name) => [
  name,
  `dark-${name}`,
])) {
  test(`visual update-${story}`, async ({ page }) => {
    await page.goto(
      `/iframe.html?id=${encodeURIComponent("rift-обновление--" + story)}&viewMode=story`,
    );
    await updateStoryReady[story.replace(/^dark-/, "")](page);
    await expect(page.locator("html")).toHaveAttribute(
      "data-theme",
      story.startsWith("dark-") ? "dark" : "light",
    );
    await page.evaluate(() => document.fonts.ready);
    await expect(page).toHaveScreenshot(`update-${story}.png`, {
      fullPage: true,
      animations: "disabled",
    });
  });
}
test("visual main-window-crash", async ({ page }) => {
  await page.goto(
    `/iframe.html?id=${encodeURIComponent("rift-сбой-интерфейса--main-window-crash")}&viewMode=story`,
  );
  await expect(page.getByRole("alert")).toBeVisible();
  await page.evaluate(() => document.fonts.ready);
  await expect(page).toHaveScreenshot("main-window-crash.png", {
    animations: "disabled",
  });
});
test("connect, disable auto, disconnect", async ({ page }) => {
  await screen(page, "disconnected");
  await page.getByRole("button", { name: "Подключиться", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "Подключено", exact: true }),
  ).toBeVisible();
  await page.getByRole("switch", { name: "Автовыбор сервера" }).click();
  await expect(
    page.getByRole("switch", { name: "Автовыбор сервера" }),
  ).toHaveAttribute("aria-checked", "false");
  await page.getByRole("button", { name: "Отключить", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "Отключено", exact: true }),
  ).toBeVisible();
});
test("filter, manual select, import, delete", async ({ page }) => {
  await screen(page, "servers");
  await page.getByRole("button", { name: "Резервная 1", exact: true }).click();
  await expect(page.getByRole("row")).toHaveCount(2);
  await page.getByRole("button", { name: "Amsterdam 01", exact: true }).click();
  await page
    .getByRole("button", { name: "Все серверы 4", exact: true })
    .click();
  await expect(page.getByRole("row")).toHaveCount(5);
  await page
    .getByRole("button", { name: "Добавить сервер", exact: true })
    .click();
  await page.getByLabel("Ссылка сервера").fill("invalid");
  await expect(page.getByRole("dialog").getByRole("alert")).toBeVisible();
  await page.getByLabel("Ссылка сервера").fill("vless://test");
  await expect(
    page.getByRole("heading", { name: "Новый сервер" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Добавить", exact: true }).click();
  await expect(page.getByRole("dialog")).toBeHidden();
  await expect(
    page.getByRole("row").filter({ hasText: "Новый сервер" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Удалить Новый сервер" }).click();
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Удалить", exact: true })
    .click();
  await expect(
    page.getByRole("row").filter({ hasText: "Новый сервер" }),
  ).toHaveCount(0);
});
test("subscription editor and journal navigation", async ({ page }) => {
  await screen(page, "servers");
  await page
    .getByRole("button", { name: "Добавить подписку", exact: true })
    .click();
  // The name is not asked for: the subscription names itself.
  await expect(page.getByLabel("Название", { exact: true })).toHaveCount(0);
  await page
    .getByLabel("Ссылка подписки", { exact: true })
    .fill("https://example.com/private");
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Добавить", exact: true })
    .click();
  await expect(page.getByRole("dialog")).toBeHidden();
  await expect(
    page.getByRole("button", { name: "example.com 0", exact: true }),
  ).toBeVisible();
  await page
    .getByRole("navigation")
    .getByRole("button", { name: /Журнал/ })
    .click();
  await page.getByRole("button", { name: "Очистить", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "В журнале пока тихо" }),
  ).toBeVisible();
});

for (const story of ["journal-long-message", "dark-journal-long-message"]) {
  test(`long journal scrolls inside the table: ${story}`, async ({
    page,
  }, info) => {
    await screen(page, story);
    const scroll = page.locator(".table-scroll");
    await expect(scroll).toBeVisible();
    const size = await scroll.evaluate((element) => ({
      content: element.scrollWidth,
      visible: element.clientWidth,
      page: document.documentElement.scrollWidth,
      viewport: window.innerWidth,
    }));
    expect(size.content).toBeGreaterThan(size.visible);
    expect(size.page).toBeLessThanOrEqual(size.viewport);
    await scroll.focus();
    await page.keyboard.press("ArrowRight");
    await expect
      .poll(() => scroll.evaluate((element) => element.scrollLeft))
      .toBeGreaterThan(0);
    await page.screenshot({ path: info.outputPath(`${story}.png`) });
  });
}

for (const story of [
  "welcome-recognised",
  "dark-welcome-recognised",
  "add-subscription",
  "dark-add-subscription",
]) {
  test(`import dialog fits the window: ${story}`, async ({ page }, info) => {
    await screen(page, story);
    const dialog = page.getByRole("dialog");
    await expect(dialog).toBeVisible();
    const welcome = story.includes("welcome");
    const field = dialog.getByLabel(
      welcome ? "Ссылка подписки или сервера" : "Ссылка подписки",
    );
    await expect(field).toBeFocused();
    if (welcome) {
      await expect(dialog.getByText("Подписка: panel.example")).toBeVisible();
      await expect(
        dialog.getByRole("button", { name: "Добавить" }),
      ).toBeEnabled();
    }
    expect(
      await page.evaluate(() => document.documentElement.scrollWidth),
    ).toBeLessThanOrEqual(page.viewportSize()!.width);
    await page.screenshot({ path: info.outputPath(`${story}.png`) });
  });
}

for (const prefix of ["", "dark-"]) {
  test(`offline regional flags preserve names and render in color: ${prefix || "light"}`, async ({
    page,
  }, info) => {
    await screen(page, `${prefix}regional-flags`);
    for (const name of ["🇫🇷 Paris · 🚀", "🇵🇱 Warsaw · ✨"])
      await expect(
        page.getByRole("button", { name, exact: true }),
      ).toBeVisible();
    await expect(page.getByRole("switch", { name: "Включена" })).toHaveCount(0);
    const glyphs = await page.evaluate(async () => {
      const faces = await document.fonts.load(
        '32px "Twemoji Country Flags"',
        "🇫🇷🇵🇱",
      );
      const canvas = document.createElement("canvas");
      canvas.width = 160;
      canvas.height = 64;
      const context = canvas.getContext("2d")!;
      context.font = '32px "Twemoji Country Flags"';
      context.fillText("🇫🇷🇵🇱", 0, 42);
      const pixels = context.getImageData(
        0,
        0,
        canvas.width,
        canvas.height,
      ).data;
      let colored = 0;
      for (let at = 0; at < pixels.length; at += 4) {
        if (
          pixels[at + 3] > 0 &&
          Math.max(pixels[at], pixels[at + 1], pixels[at + 2]) -
            Math.min(pixels[at], pixels[at + 1], pixels[at + 2]) >
            50
        )
          colored += 1;
      }
      const fontRequests = performance
        .getEntriesByType("resource")
        .filter(
          (entry) =>
            entry.name.includes("TwemojiCountryFlags") &&
            new URL(entry.name).origin === location.origin,
        );
      return {
        faces: faces.length,
        colored,
        localRequests: fontRequests.length,
      };
    });
    expect(glyphs.faces).toBeGreaterThan(0);
    expect(glyphs.colored).toBeGreaterThan(10);
    expect(glyphs.localRequests).toBeGreaterThan(0);
    await page.screenshot({
      path: info.outputPath(`${prefix}regional-flags.png`),
    });
  });
  test(`only journal level is centered: ${prefix || "light"}`, async ({
    page,
  }, info) => {
    await screen(page, `${prefix}journal-level-centered`);
    const level = page.getByRole("columnheader", { name: "Уровень" });
    await expect(level).toHaveCSS("text-align", "center");
    const table = level.locator("xpath=ancestor::table");
    const headers = table.getByRole("columnheader");
    for (let column = 0; column < (await headers.count()); column += 1) {
      const heading = headers.nth(column);
      const alignment =
        (await heading.textContent()) === "Уровень" ? "center" : "left";
      await expect(heading).toHaveCSS("text-align", alignment);
      for (const cell of await table
        .locator(`tbody tr td:nth-child(${column + 1})`)
        .all())
        await expect(cell).toHaveCSS("text-align", alignment);
    }
    await page.screenshot({
      path: info.outputPath(`${prefix}journal-level-centered.png`),
    });
  });
}

for (const prefix of ["", "dark-"]) {
  test(`journal disclosure stays with its record: ${prefix || "system"}`, async ({
    page,
  }) => {
    await screen(page, `${prefix}journal-disclosure-identity`);
    const first = page
      .getByRole("row")
      .filter({ hasText: "Ошибка интерфейса" });
    const second = page.getByRole("row").filter({ hasText: "Ошибка подписки" });
    await first.locator("summary").focus();
    await page.keyboard.press("Enter");
    await expect(first.locator("details")).toHaveAttribute("open");
    await expect(first.locator("pre")).toHaveText(
      "TypeError: boom\n    at refresh (assets/index-ab12.js:12:34)",
    );
    await page.getByRole("button", { name: "Подписки", exact: true }).click();
    await expect(second.locator("details")).not.toHaveAttribute("open");
    await page.getByRole("button", { name: "Все", exact: true }).click();
    await first.locator("summary").click();
    await page
      .getByRole("button", { name: "Добавить запись (сценарий)" })
      .click();
    await expect(first.locator("details")).toHaveAttribute("open");
    const added = page.getByRole("row").filter({ hasText: "Новая ошибка" });
    await expect(added.locator("details")).not.toHaveAttribute("open");
    await page.getByRole("button", { name: "Ротация (сценарий)" }).click();
    await expect(first).toHaveCount(0);
    await expect(second.locator("details")).not.toHaveAttribute("open");
    await expect(added.locator("details")).not.toHaveAttribute("open");
  });
}

// New contract states use the same two viewport projects as the established screens.
const contractStoryReady: Record<string, (page: Page) => Promise<unknown>> = {
  "journal-details": (page) =>
    expect(
      page.getByText(
        "Xray parser: missing field settings.vnext[0].users at line 4 column 8",
      ),
    ).toBeVisible(),
  "journal-export-warning": (page) =>
    expect(
      page.getByRole("heading", {
        name: "Сохранить локальную диагностику?",
      }),
    ).toBeVisible(),
  "journal-crash-identity": async (page) => {
    await expect(page.getByRole("cell", { name: /^Новая ошибка/ })).toBeVisible();
    await expect(
      page.getByRole("cell", { name: /^Ошибка интерфейса/ }),
    ).toHaveCount(1);
    await expect(page.getByText("TypeError: repeated crash")).toBeHidden();
  },
  "engine-failures": (page) =>
    expect(page.getByText("Xray неожиданно завершился")).toBeVisible(),
  "subscription-filtered": (page) =>
    expect(
      page.getByRole("heading", {
        name: "Фильтры скрыли все серверы подписки",
      }),
    ).toBeVisible(),
  "subscription-invalid": (page) =>
    expect(
      page.getByRole("heading", {
        name: "Подписка не содержит серверов",
      }),
    ).toBeVisible(),
  "subscription-never-loaded": (page) =>
    expect(
      page.getByRole("heading", {
        name: "Серверы подписки ещё не загружены",
      }),
    ).toBeVisible(),
  "subscription-loading": (page) =>
    expect(
      page.getByRole("heading", {
        name: "Загружаем серверы…",
      }),
    ).toBeVisible(),
  "connections-loading": (page) =>
    expect(page.getByText("Читаем соединения…")).toBeVisible(),
  "connections-empty": (page) =>
    expect(
      page.getByRole("heading", {
        name: "Активных соединений нет",
      }),
    ).toBeVisible(),
  "connections-no-matches": (page) =>
    expect(
      page.getByRole("heading", {
        name: "Ничего не найдено",
      }),
    ).toBeVisible(),
  "update-checking": (page) =>
    expect(
      page.getByRole("button", {
        name: "Проверить обновления",
      }),
    ).toBeDisabled(),
  "update-check-error": (page) =>
    expect(page.getByText("Сеть недоступна")).toBeVisible(),
  "update-no-update": (page) =>
    expect(page.getByText("У вас последняя версия.")).toBeVisible(),
};
for (const name of [
  "qr-file-import",
  "qr-clipboard-import",
  "pending-deep-link-import",
  "live-deep-link-import",
]) {
  contractStoryReady[name] = async (page) => {
    const dialog = page.getByRole("dialog");
    await expect(
      dialog.getByRole("heading", { name: "Contract", exact: true }),
    ).toBeVisible();
    await expect(
      dialog.getByRole("button", { name: "Добавить", exact: true }),
    ).toBeEnabled();
  };
}
for (const name of Object.keys(contractStoryReady)) {
  for (const prefix of ["", "dark-"]) {
    const story = `${prefix}${name}`;
    test(`visual ${story}`, async ({ page }) => {
      await screen(page, story);
      await contractStoryReady[name](page);
      await expect(page.locator("html")).toHaveAttribute(
        "data-theme",
        prefix ? "dark" : "light",
      );
      await expect(page).toHaveScreenshot(`${story}.png`, {
        fullPage: true,
        animations: "disabled",
      });
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth,
        ),
      ).toBe(true);
    });
  }
}

for (const initial of ["light", "dark"] as const) {
  test(`visual system-theme-${initial}`, async ({ page }) => {
    await page.emulateMedia({ colorScheme: initial });
    await screen(page, "system-theme");
    await expect(page.locator("html")).toHaveAttribute("data-theme", initial);
    await expect(page).toHaveScreenshot(`system-theme-${initial}.png`, {
      fullPage: true,
      animations: "disabled",
    });
    const changed = initial === "light" ? "dark" : "light";
    // Native media change, no synthetic matchMedia implementation or remount.
    await page.emulateMedia({ colorScheme: changed });
    await expect(page.locator("html")).toHaveAttribute("data-theme", changed);
    await page.emulateMedia({ colorScheme: initial });
    await expect(page.locator("html")).toHaveAttribute("data-theme", initial);
  });
}

for (const prefix of ["", "dark-"]) {
  test(`visual standalone-${prefix}statuses`, async ({ page }) => {
    await page.goto(
      `/iframe.html?id=${encodeURIComponent(`rift-компоненты--${prefix}statuses`)}&viewMode=story`,
    );
    await expect(page.locator(".badge").first()).toBeVisible();
    await expect(page.locator("html")).toHaveAttribute(
      "data-theme",
      prefix ? "dark" : "light",
    );
    await page.evaluate(() => document.fonts.ready);
    await expect(page).toHaveScreenshot(`standalone-${prefix}statuses.png`, {
      fullPage: true,
      animations: "disabled",
    });
  });
}
test("visual dark-main-window-crash", async ({ page }) => {
  await page.goto(
    `/iframe.html?id=${encodeURIComponent("rift-сбой-интерфейса--dark-main-window-crash")}&viewMode=story`,
  );
  await expect(page.getByRole("alert")).toContainText(
    "Интерфейс Rift остановился",
  );
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await page.evaluate(() => document.fonts.ready);
  await expect(page).toHaveScreenshot("dark-main-window-crash.png", {
    fullPage: true,
    animations: "disabled",
  });
});
