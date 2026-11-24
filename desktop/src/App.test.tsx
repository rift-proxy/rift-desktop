import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { it, expect, vi } from "vitest";
import { App } from "./App";
import { BackendProvider } from "./backend/provider";
import { MockBackend, fixture } from "./testing/mock";
import { SETTINGS_SAVE_DELAY_MS } from "./useSettingsDraft";
it("connects, toggles automatic selection and disconnects through adapter", async () => {
  const user = userEvent.setup();
  render(
    <BackendProvider
      client={
        new MockBackend({
          ...fixture,
          status: "disconnected",
          runtime_active: false,
        })
      }
    >
      <App />
    </BackendProvider>,
  );
  await user.click(await screen.findByRole("button", { name: "Подключиться" }));
  expect(
    await screen.findByRole("heading", { name: "Подключено" }),
  ).toBeInTheDocument();
  await user.click(screen.getByRole("switch"));
  await waitFor(() =>
    expect(screen.getByRole("switch")).toHaveAttribute("aria-checked", "false"),
  );
  await user.click(screen.getByRole("button", { name: "Отключить" }));
  expect(
    await screen.findByRole("heading", { name: "Отключено" }),
  ).toBeInTheDocument();
});
it("says the connection is lost, in the heading and on the other pages, with no error popup", async () => {
  const user = userEvent.setup();
  render(
    <BackendProvider
      client={
        new MockBackend({
          ...fixture,
          status: "lost",
          settings: { ...fixture.settings, auto_select: false },
        })
      }
    >
      <App />
    </BackendProvider>,
  );
  expect(
    await screen.findByRole("heading", { name: "Соединение потеряно" }),
  ).toBeVisible();
  expect(screen.getByText(/Выбранный сервер не отвечает/)).toBeVisible();
  expect(screen.queryByRole("alert")).toBeNull();
  await user.click(screen.getByRole("button", { name: "Серверы" }));
  expect(document.querySelector(".link-status")).toHaveTextContent(
    "Соединение потеряно",
  );
});
it("tells that TUN asks Windows for confirmation instead of asking for administrator rights", async () => {
  render(
    <BackendProvider client={new MockBackend()}>
      <App initialPage="settings" />
    </BackendProvider>,
  );
  expect(
    await screen.findByText(/TUN запросит подтверждение Windows \(UAC\)/),
  ).toBeVisible();
  expect(screen.queryByText(/от имени администратора/)).not.toBeInTheDocument();
});
it("offers the interface languages by their own names with the flag of the country, Russian only for now", async () => {
  render(
    <BackendProvider client={new MockBackend()}>
      <App initialPage="settings" />
    </BackendProvider>,
  );
  const language = await screen.findByRole("combobox", { name: "Язык" });
  expect(language).toHaveValue("ru");
  expect(
    within(language)
      .getAllByRole("option")
      .map((option) => option.textContent),
  ).toEqual(["🇷🇺 Русский"]);
});
it("filters servers by source and adds a parsed manual server", async () => {
  const user = userEvent.setup();
  render(
    <BackendProvider client={new MockBackend()}>
      <App initialPage="servers" />
    </BackendProvider>,
  );
  await user.click(await screen.findByRole("button", { name: /^Резервная 1/ }));
  expect(
    screen.queryByRole("button", { name: "Frankfurt 01" }),
  ).not.toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Все серверы 4" }));
  await user.click(screen.getByRole("button", { name: "Добавить сервер" }));
  await user.type(screen.getByLabelText("Ссылка сервера"), "vless://test");
  await screen.findByText("Новый сервер");
  await user.click(screen.getByRole("button", { name: "Добавить" }));
  await waitFor(() =>
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
  );
  expect(
    screen.getByRole("button", { name: "Новый сервер" }),
  ).toBeInTheDocument();
});

it("shows subscription refresh progress and disables refresh controls", async () => {
  render(
    <BackendProvider client={new MockBackend({ ...fixture, refreshing: true })}>
      <App initialPage="servers" />
    </BackendProvider>,
  );
  expect(await screen.findByText("Обновление подписок…")).toBeVisible();
  expect(screen.getByRole("button", { name: "Обновить все" })).toBeDisabled();
  expect(
    screen.getByRole("button", { name: "Обновить Основная" }),
  ).toBeDisabled();
  expect(
    screen.getByRole("button", { name: "Обновить Резервная" }),
  ).toBeDisabled();
});

it("shows localized failure text and hides an unknown failure code", async () => {
  render(
    <BackendProvider
      client={
        new MockBackend({
          ...fixture,
          servers: [
            {
              ...fixture.servers[0],
              status: "failed",
              failure_code: "future_dependency_detail",
            },
          ],
        })
      }
    >
      <App initialPage="servers" />
    </BackendProvider>,
  );
  expect(await screen.findByText("Не удалось проверить сервер")).toBeVisible();
  expect(
    screen.queryByText("future_dependency_detail"),
  ).not.toBeInTheDocument();
});

it("keeps the add dialog open and shows a backend save failure inside it", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  vi.spyOn(backend, "execute").mockRejectedValue("Не удалось сохранить данные");
  render(
    <BackendProvider client={backend}>
      <App initialPage="servers" />
    </BackendProvider>,
  );
  await user.click(
    await screen.findByRole("button", { name: "Добавить сервер" }),
  );
  await user.type(screen.getByLabelText("Ссылка сервера"), "vless://test");
  await screen.findByText("Новый сервер");
  await user.click(screen.getByRole("button", { name: "Добавить" }));
  expect(
    await within(screen.getByRole("dialog")).findByRole("alert"),
  ).toHaveTextContent("Не удалось сохранить данные");
});

it("shows the host-reported version in the footer", async () => {
  const backend = new MockBackend();
  backend.appVersion = () => Promise.resolve("42.6.3");
  render(
    <BackendProvider client={backend}>
      <App />
    </BackendProvider>,
  );
  expect(await screen.findByText("Rift 42.6.3")).toBeVisible();
});

it("shows no version number of its own when the host cannot tell it", async () => {
  const backend = new MockBackend(fixture);
  const asked = vi
    .spyOn(backend, "appVersion")
    .mockRejectedValue(new Error("unavailable"));
  render(
    <BackendProvider client={backend}>
      <App />
    </BackendProvider>,
  );
  await waitFor(() => expect(asked).toHaveBeenCalled());
  expect(screen.getByRole("contentinfo")).toHaveTextContent(/^Rift$/);
});

it("shows an update banner and installs it on confirmation", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  render(
    <BackendProvider client={backend}>
      <App />
    </BackendProvider>,
  );
  // A live Tauri event, not a replayed one: push only after the window has subscribed.
  await screen.findByRole("region", { name: "Подключение" });
  backend.pushUpdate({ version: "0.2.0", notes: null, pub_date: null });
  await screen.findByText("Доступна версия 0.2.0.");
  await user.click(screen.getByRole("button", { name: "Установить" }));
  await user.click(
    screen.getByRole("button", { name: "Установить и перезапустить" }),
  );
  await waitFor(() => expect(backend.installed).toBe(true));
});

it("checks for updates from Settings and reports there is nothing newer", async () => {
  const user = userEvent.setup();
  render(
    <BackendProvider client={new MockBackend()}>
      <App initialPage="settings" />
    </BackendProvider>,
  );
  await user.click(
    await screen.findByRole("button", { name: "Проверить обновления" }),
  );
  expect(await screen.findByText("У вас последняя версия.")).toBeVisible();
});

it("does not resurface a dismissed version on a manual recheck from Settings", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  render(
    <BackendProvider client={backend}>
      <App />
    </BackendProvider>,
  );
  await screen.findByRole("region", { name: "Подключение" });
  backend.pushUpdate({ version: "0.2.0", notes: null, pub_date: null });
  await screen.findByText("Доступна версия 0.2.0.");
  await user.click(screen.getByRole("button", { name: "Закрыть уведомление" }));
  await waitFor(() =>
    expect(
      screen.queryByText("Доступна версия 0.2.0."),
    ).not.toBeInTheDocument(),
  );
  await user.click(screen.getByRole("button", { name: "Настройки" }));
  await user.click(
    await screen.findByRole("button", { name: "Проверить обновления" }),
  );
  expect(await screen.findByText("У вас последняя версия.")).toBeVisible();

  // Per-version suppression, not blanket amnesia: set the backend's latest info directly
  // (not pushUpdate, which would also fire the push event) so only the manual check's own
  // return value — not a side channel — can make the newer version appear here.
  backend.updateInfo = { version: "0.3.0", notes: null, pub_date: null };
  await user.click(
    screen.getByRole("button", { name: "Проверить обновления" }),
  );
  // Appears twice once resurfaced: the Settings panel's own result and the App-wide banner,
  // which both read the same shared update state.
  await waitFor(() =>
    expect(screen.getAllByText("Доступна версия 0.3.0.")).toHaveLength(2),
  );
});

it("shows an error in Settings when checking for updates fails", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  vi.spyOn(backend, "checkForUpdate").mockRejectedValue("Сеть недоступна");
  render(
    <BackendProvider client={backend}>
      <App initialPage="settings" />
    </BackendProvider>,
  );
  await user.click(
    await screen.findByRole("button", { name: "Проверить обновления" }),
  );
  expect(await screen.findByText("Сеть недоступна")).toBeVisible();
  expect(screen.queryByText("У вас последняя версия.")).not.toBeInTheDocument();
});

it("warns about plaintext secrets before exporting a profile", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  const exportProfile = vi.spyOn(backend, "exportProfile");
  render(
    <BackendProvider client={backend}>
      <App initialPage="settings" />
    </BackendProvider>,
  );

  await user.click(
    await screen.findByRole("button", { name: "Экспортировать" }),
  );
  expect(exportProfile).not.toHaveBeenCalled();
  const dialog = screen.getByRole("dialog");
  expect(dialog).toHaveTextContent("Файл будет незашифрованным");
  expect(dialog).toHaveTextContent("пароли серверов и ссылки подписок");

  await user.click(
    within(dialog).getByRole("button", { name: "Экспортировать" }),
  );
  await waitFor(() => expect(exportProfile).toHaveBeenCalledOnce());
  expect(await screen.findByText("Профиль экспортирован.")).toBeVisible();
});

it("previews a validated profile before replacing current data", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  const importProfile = vi.spyOn(backend, "importProfile");
  render(
    <BackendProvider client={backend}>
      <App initialPage="settings" />
    </BackendProvider>,
  );

  await user.click(
    await screen.findByRole("button", { name: "Импортировать" }),
  );
  const dialog = await screen.findByRole("dialog");
  expect(dialog).toHaveTextContent("Проверен профиль версии 2");
  expect(dialog).toHaveTextContent("Серверы4");
  expect(dialog).toHaveTextContent("Подписки2");
  expect(importProfile).not.toHaveBeenCalled();

  await user.click(
    within(dialog).getByRole("button", { name: "Импортировать" }),
  );
  await waitFor(() => expect(importProfile).toHaveBeenCalledWith(1));
  expect(await screen.findByText("Профиль импортирован.")).toBeVisible();
});

it("keeps the import preview open when profile replacement fails", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  vi.spyOn(backend, "importProfile").mockRejectedValue(
    "Не удалось применить профиль; текущие данные сохранены",
  );
  render(
    <BackendProvider client={backend}>
      <App initialPage="settings" />
    </BackendProvider>,
  );

  await user.click(
    await screen.findByRole("button", { name: "Импортировать" }),
  );
  const dialog = await screen.findByRole("dialog");
  await user.click(
    within(dialog).getByRole("button", { name: "Импортировать" }),
  );
  expect(
    await within(dialog).findByText(
      "Не удалось применить профиль; текущие данные сохранены",
    ),
  ).toBeVisible();
  expect(screen.getByRole("dialog")).toBeVisible();
});

it("keeps the import result, the dialog opener and the new settings when the imported profile changes settings", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  // A restore on a clean install: the file's settings differ from the current ones, so the
  // state event that follows the import changes `Snapshot.settings` while Settings is open.
  backend.profileFile = {
    settings: { ...fixture.settings, proxy_port: 3090, auto_select: false },
    servers: fixture.servers.slice(0, 2),
    subscriptions: fixture.subscriptions.slice(0, 1),
    selected: null,
    routing: structuredClone(fixture.routing),
    servers_sort: { ...fixture.servers_sort },
  };
  render(
    <BackendProvider client={backend}>
      <App initialPage="settings" />
    </BackendProvider>,
  );

  const opener = await screen.findByRole("button", { name: "Импортировать" });
  await user.click(opener);
  const dialog = await screen.findByRole("dialog");
  expect(dialog).toHaveTextContent("Серверы2");
  await user.click(
    within(dialog).getByRole("button", { name: "Импортировать" }),
  );

  expect(await screen.findByText("Профиль импортирован.")).toBeVisible();
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  // The same Settings screen stayed mounted, so the control that opened the dialog is alive
  // and gets the focus back.
  expect(opener).toBeInTheDocument();
  expect(opener).toHaveFocus();
  // The form shows what was imported, not the draft from before the import.
  expect(screen.getByLabelText("Порт", { exact: true })).toHaveValue(3090);
  expect(
    screen.getByRole("switch", { name: "Включить автовыбор" }),
  ).toHaveAttribute("aria-checked", "false");
});

const savedSettings = (spy: { mock: { calls: unknown[][] } }) =>
  spy.mock.calls
    .map(([action]) => action as { type: string; settings?: unknown })
    .filter((action) => action.type === "save_settings");

it("saves settings by itself: a list or a switch at once, no save button", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  const execute = vi.spyOn(backend, "execute");
  render(
    <BackendProvider client={backend}>
      <App initialPage="settings" />
    </BackendProvider>,
  );
  const mode = await screen.findByLabelText("Режим работы");
  expect(
    screen.queryByRole("button", { name: "Сохранить настройки" }),
  ).not.toBeInTheDocument();
  await user.selectOptions(mode, "tun");
  await waitFor(() => expect(savedSettings(execute)).toHaveLength(1));
  expect(savedSettings(execute)[0]).toMatchObject({
    settings: { mode: "tun" },
  });
  await user.click(screen.getByRole("switch", { name: "Включить автовыбор" }));
  await waitFor(() => expect(savedSettings(execute)).toHaveLength(2));
  expect(savedSettings(execute)[1]).toMatchObject({
    settings: { mode: "tun", auto_select: !fixture.settings.auto_select },
  });
});

it("saves a typed number once, after the pause, and at once when the field loses focus", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  const execute = vi.spyOn(backend, "execute");
  render(
    <BackendProvider client={backend}>
      <App initialPage="settings" />
    </BackendProvider>,
  );
  const port = await screen.findByLabelText("Порт", { exact: true });
  await user.clear(port);
  await user.type(port, "4000");
  // Four keystrokes are one change, not four saves.
  expect(savedSettings(execute)).toHaveLength(0);
  await waitFor(() => expect(savedSettings(execute)).toHaveLength(1), {
    timeout: SETTINGS_SAVE_DELAY_MS * 4,
  });
  expect(savedSettings(execute)[0]).toMatchObject({
    settings: { proxy_port: 4000 },
  });

  await user.clear(port);
  await user.type(port, "4100");
  await user.tab();
  // Leaving the field does not wait for the pause.
  expect(savedSettings(execute)).toHaveLength(2);
  expect(savedSettings(execute)[1]).toMatchObject({
    settings: { proxy_port: 4100 },
  });
});

it("does not send an invalid value and tells why", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  const execute = vi.spyOn(backend, "execute");
  render(
    <BackendProvider client={backend}>
      <App initialPage="settings" />
    </BackendProvider>,
  );
  const port = await screen.findByLabelText("Порт", { exact: true });
  await user.clear(port);
  expect(await screen.findByText("От 1 до 65535")).toBeInTheDocument();
  await user.tab();
  await new Promise((r) => setTimeout(r, SETTINGS_SAVE_DELAY_MS + 200));
  expect(savedSettings(execute)).toHaveLength(0);

  // The API port may not equal the proxy port: caught before the core has to refuse it.
  await user.type(port, String(fixture.settings.api_port));
  expect(
    await screen.findByText("Порт API должен отличаться от порта прокси"),
  ).toBeInTheDocument();
  await user.tab();
  expect(savedSettings(execute)).toHaveLength(0);
});

it("keeps the thresholds for leaving a working server under Auto Select, both of them", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  const execute = vi.spyOn(backend, "execute");
  render(
    <BackendProvider client={backend}>
      <App initialPage="settings" />
    </BackendProvider>,
  );
  const percent = await screen.findByLabelText("Порог смены сервера, %");
  const ms = screen.getByLabelText("Порог смены сервера, мс");
  expect(percent).toHaveValue(30);
  expect(ms).toHaveValue(50);
  await user.clear(percent);
  await user.type(percent, "40");
  await user.tab();
  expect(savedSettings(execute).at(-1)).toMatchObject({
    settings: { switch_threshold_percent: 40, switch_threshold_ms: 50 },
  });
  await user.clear(ms);
  await user.type(ms, "10001");
  expect(await screen.findByText("От 0 до 10000")).toBeInTheDocument();
  await user.tab();
  expect(savedSettings(execute)).toHaveLength(1);
  await user.clear(ms);
  await user.type(ms, "0");
  await user.tab();
  expect(savedSettings(execute).at(-1)).toMatchObject({
    settings: { switch_threshold_percent: 40, switch_threshold_ms: 0 },
  });
});

it("shows the settings of the fast and the full check, with their limits, and saves them", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  const execute = vi.spyOn(backend, "execute");
  render(
    <BackendProvider client={backend}>
      <App initialPage="settings" />
    </BackendProvider>,
  );
  const fast = await screen.findByLabelText("Быстрая проверка, сек");
  const full = screen.getByLabelText("Полная проверка, мин");
  const pool = screen.getByLabelText("Серверов в запасе");
  const percent = screen.getByLabelText("Допуск ухудшения, %");
  const cap = screen.getByLabelText("Предел допуска, мс");
  expect(fast).toHaveValue(60);
  expect(full).toHaveValue(10);
  expect(pool).toHaveValue(3);
  expect(percent).toHaveValue(50);
  expect(cap).toHaveValue(1500);
  // The long explanation of the two checks is gone.
  expect(
    screen.queryByText(/плюс время самой проверки/),
  ).not.toBeInTheDocument();

  await user.clear(fast);
  await user.type(fast, "49");
  expect(await screen.findByText("От 50 до 300")).toBeInTheDocument();
  await user.tab();
  expect(savedSettings(execute)).toHaveLength(0);
  await user.clear(fast);
  await user.type(fast, "120");
  await user.clear(pool);
  await user.type(pool, "6");
  expect(await screen.findByText("От 1 до 5")).toBeInTheDocument();
  await user.clear(pool);
  await user.type(pool, "4");
  await user.clear(full);
  await user.type(full, "20");
  await user.tab();
  expect(savedSettings(execute).at(-1)).toMatchObject({
    settings: { interval_secs: 120, pool_size: 4, full_interval_mins: 20 },
  });
});

it("keeps the routing databases up to date by default and lets the user turn that off", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  const execute = vi.spyOn(backend, "execute");
  render(
    <BackendProvider client={backend}>
      <App initialPage="settings" />
    </BackendProvider>,
  );
  const update = await screen.findByRole("switch", {
    name: "Обновлять базы маршрутизации",
  });
  expect(update).toHaveAttribute("aria-checked", "true");
  expect(screen.getByText("Базы geosite и geoip")).toBeInTheDocument();
  await user.click(update);
  await waitFor(() => expect(update).toHaveAttribute("aria-checked", "false"));
  expect(savedSettings(execute).at(-1)).toEqual({
    type: "save_settings",
    settings: { ...fixture.settings, geodata_auto_update: false },
  });
  await user.click(update);
  await waitFor(() => expect(update).toHaveAttribute("aria-checked", "true"));
  expect(savedSettings(execute).at(-1)).toEqual({
    type: "save_settings",
    settings: { ...fixture.settings, geodata_auto_update: true },
  });
});

it("keeps what is being typed while earlier saves answer, and takes new backend settings afterwards", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  const execute = vi.spyOn(backend, "execute");
  render(
    <BackendProvider client={backend}>
      <App initialPage="settings" />
    </BackendProvider>,
  );
  const port = await screen.findByLabelText("Порт", { exact: true });
  await user.clear(port);
  await user.type(port, "4000");

  // Every snapshot is a new object: an unrelated one must not erase the unsaved draft.
  await backend.execute({ type: "check_now" });
  expect(screen.getByLabelText("Порт", { exact: true })).toHaveValue(4000);

  await waitFor(() => expect(savedSettings(execute)).toHaveLength(1), {
    timeout: SETTINGS_SAVE_DELAY_MS * 4,
  });
  // Settings changed elsewhere (an imported profile) replace the form once nothing waits.
  await backend.execute({
    type: "save_settings",
    settings: { ...fixture.settings, proxy_port: 5000 },
  });
  await waitFor(() =>
    expect(screen.getByLabelText("Порт", { exact: true })).toHaveValue(5000),
  );
});

it("puts the form back to the saved values when the core refuses the change", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  const execute = vi.spyOn(backend, "execute");
  execute.mockRejectedValueOnce("Порт занят");
  render(
    <BackendProvider client={backend}>
      <App initialPage="settings" />
    </BackendProvider>,
  );
  const mode = await screen.findByLabelText("Режим работы");
  await user.selectOptions(mode, "tun");
  expect(await screen.findByText("Порт занят")).toBeInTheDocument();
  await waitFor(() =>
    expect(screen.getByLabelText("Режим работы")).toHaveValue(
      fixture.settings.mode,
    ),
  );
});

it("saves a value that was still waiting for its pause when the page is left", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  const execute = vi.spyOn(backend, "execute");
  const { unmount } = render(
    <BackendProvider client={backend}>
      <App initialPage="settings" />
    </BackendProvider>,
  );
  const port = await screen.findByLabelText("Порт", { exact: true });
  await user.clear(port);
  await user.type(port, "4200");
  expect(savedSettings(execute)).toHaveLength(0);
  unmount();
  expect(savedSettings(execute)).toHaveLength(1);
  expect(savedSettings(execute)[0]).toMatchObject({
    settings: { proxy_port: 4200 },
  });
});

for (const [operation, openLabel, step] of [
  ["export", "Экспортировать", "export"],
  ["import", "Импортировать", "import"],
] as const) {
  it(`does not let Escape dismiss the profile ${operation} dialog while it runs`, async () => {
    const user = userEvent.setup();
    const backend = new MockBackend();
    const finish = backend.holdProfile(step);
    render(
      <BackendProvider client={backend}>
        <App initialPage="settings" />
      </BackendProvider>,
    );
    await user.click(await screen.findByRole("button", { name: openLabel }));
    const dialog = await screen.findByRole("dialog");
    await user.click(within(dialog).getByRole("button", { name: openLabel }));
    await waitFor(() =>
      expect(
        within(dialog).getByRole("button", { name: "Отмена" }),
      ).toBeDisabled(),
    );

    // Escape reaches the dialog as `cancel`: the operation keeps running behind it.
    fireEvent(dialog, new Event("cancel", { cancelable: true }));
    expect(screen.getByRole("dialog")).toBeVisible();
    expect(
      within(dialog).getByRole("button", { name: "Закрыть диалог" }),
    ).toBeDisabled();

    finish();
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
  });
}

it("shows a failed export inside the open dialog, once, and lets the user retry", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  backend.failProfile(
    "export",
    "Не удалось сохранить профиль",
    "Ошибка чтения или сохранения данных",
  );
  render(
    <BackendProvider client={backend}>
      <App initialPage="settings" />
    </BackendProvider>,
  );

  await user.click(
    await screen.findByRole("button", { name: "Экспортировать" }),
  );
  const dialog = screen.getByRole("dialog");
  await user.click(
    within(dialog).getByRole("button", { name: "Экспортировать" }),
  );

  // The failure is inside the dialog the user is looking at, announced as an alert, and the
  // warning stays; the Settings panel behind the modal does not repeat it.
  const inline = await within(dialog).findByText(
    "Не удалось сохранить профиль",
  );
  expect(inline.closest('[role="alert"]')).not.toBeNull();
  expect(dialog).toHaveTextContent("Файл будет незашифрованным");
  expect(screen.getAllByText("Не удалось сохранить профиль")).toHaveLength(1);
  expect(
    within(dialog).getByRole("button", { name: "Экспортировать" }),
  ).toBeEnabled();
  expect(backend.profileExported).toBe(false);

  await user.click(
    within(dialog).getByRole("button", { name: "Экспортировать" }),
  );
  expect(await screen.findByText("Профиль экспортирован.")).toBeVisible();
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  expect(
    screen.queryByText("Не удалось сохранить профиль"),
  ).not.toBeInTheDocument();
});

it("does not carry an export failure over to the next time the dialog opens", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  backend.failProfile("export", "Не удалось сохранить профиль");
  render(
    <BackendProvider client={backend}>
      <App initialPage="settings" />
    </BackendProvider>,
  );
  await user.click(
    await screen.findByRole("button", { name: "Экспортировать" }),
  );
  await user.click(
    within(screen.getByRole("dialog")).getByRole("button", {
      name: "Экспортировать",
    }),
  );
  await screen.findByText("Не удалось сохранить профиль");
  await user.click(screen.getByRole("button", { name: "Отмена" }));
  // After Cancel the failure is shown once, in the panel.
  expect(await screen.findByText("Не удалось сохранить профиль")).toBeVisible();

  await user.click(screen.getByRole("button", { name: "Экспортировать" }));
  expect(
    within(screen.getByRole("dialog")).queryByText(
      "Не удалось сохранить профиль",
    ),
  ).not.toBeInTheDocument();
});

it("keeps the exact profile text in the dialog and the core error in the global banner until a retry succeeds", async () => {
  // Decision: `Snapshot.error` is the core's last error and stays in the
  // global banner like for every other command; the dialog adds the actionable text. React
  // cannot tell which backend error belongs to its own operation, so it does not hide either:
  // the core clears the error a profile step left when a later profile step succeeds.
  const user = userEvent.setup();
  const backend = new MockBackend();
  backend.failProfile(
    "import",
    "Не удалось применить профиль; текущие данные сохранены",
    "Ошибка чтения или сохранения данных",
  );
  render(
    <BackendProvider client={backend}>
      <App initialPage="settings" />
    </BackendProvider>,
  );
  await user.click(
    await screen.findByRole("button", { name: "Импортировать" }),
  );
  const dialog = await screen.findByRole("dialog");
  await user.click(
    within(dialog).getByRole("button", { name: "Импортировать" }),
  );

  expect(
    await within(dialog).findByText(
      "Не удалось применить профиль; текущие данные сохранены",
    ),
  ).toBeVisible();
  expect(
    await screen.findByText("Ошибка чтения или сохранения данных"),
  ).toBeInTheDocument();
  expect(dialog).not.toHaveTextContent("Ошибка чтения или сохранения данных");
  expect(
    screen.getAllByText(
      "Не удалось применить профиль; текущие данные сохранены",
    ),
  ).toHaveLength(1);

  // The same checked file is applied on retry, and the banner does not contradict the result.
  await user.click(
    within(dialog).getByRole("button", { name: "Импортировать" }),
  );
  expect(await screen.findByText("Профиль импортирован.")).toBeVisible();
  await waitFor(() =>
    expect(
      screen.queryByText("Ошибка чтения или сохранения данных"),
    ).not.toBeInTheDocument(),
  );
});

it("drops the global banner when a failed export succeeds on retry", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  backend.failProfile(
    "export",
    "Не удалось сохранить профиль",
    "Ошибка чтения или сохранения данных",
  );
  render(
    <BackendProvider client={backend}>
      <App initialPage="settings" />
    </BackendProvider>,
  );
  await user.click(
    await screen.findByRole("button", { name: "Экспортировать" }),
  );
  const dialog = screen.getByRole("dialog");
  await user.click(
    within(dialog).getByRole("button", { name: "Экспортировать" }),
  );
  expect(
    await screen.findByText("Ошибка чтения или сохранения данных"),
  ).toBeVisible();

  await user.click(
    within(dialog).getByRole("button", { name: "Экспортировать" }),
  );
  expect(await screen.findByText("Профиль экспортирован.")).toBeVisible();
  await waitFor(() =>
    expect(
      screen.queryByText("Ошибка чтения или сохранения данных"),
    ).not.toBeInTheDocument(),
  );
});

it("keeps an unrelated core error when a failed profile operation succeeds on retry", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  backend.failProfile(
    "export",
    "Не удалось сохранить профиль",
    "Ошибка чтения или сохранения данных",
  );
  render(
    <BackendProvider client={backend}>
      <App initialPage="settings" />
    </BackendProvider>,
  );
  await user.click(
    await screen.findByRole("button", { name: "Экспортировать" }),
  );
  const dialog = screen.getByRole("dialog");
  await user.click(
    within(dialog).getByRole("button", { name: "Экспортировать" }),
  );
  await screen.findByText("Ошибка чтения или сохранения данных");
  // A real background failure arrives while the export dialog is still open.
  act(() => backend.recordCoreError("Процесс sing-box неожиданно завершился"));
  expect(
    await screen.findByText("Процесс sing-box неожиданно завершился"),
  ).toBeVisible();

  await user.click(
    within(dialog).getByRole("button", { name: "Экспортировать" }),
  );
  expect(await screen.findByText("Профиль экспортирован.")).toBeVisible();
  expect(
    screen.getByText("Процесс sing-box неожиданно завершился"),
  ).toBeVisible();
});

it("keeps an earlier core error through a successful profile import", async () => {
  const user = userEvent.setup();
  render(
    <BackendProvider
      client={new MockBackend({ ...fixture, error: "Нет доступных серверов" })}
    >
      <App initialPage="settings" />
    </BackendProvider>,
  );
  expect(await screen.findByText("Нет доступных серверов")).toBeVisible();
  await user.click(
    await screen.findByRole("button", { name: "Импортировать" }),
  );
  await user.click(
    within(await screen.findByRole("dialog")).getByRole("button", {
      name: "Импортировать",
    }),
  );
  expect(await screen.findByText("Профиль импортирован.")).toBeVisible();
  expect(screen.getByText("Нет доступных серверов")).toBeVisible();
});

it("has none of the template decoration on any page", async () => {
  const user = userEvent.setup();
  const { container } = render(
    <BackendProvider client={new MockBackend()}>
      <App />
    </BackendProvider>,
  );
  await screen.findByRole("region", { name: "Подключение" });
  for (const page of ["Подключение", "Серверы", "Настройки", "Журнал"]) {
    await user.click(screen.getByRole("button", { name: page }));
    // The connection page has no heading: its name is the tab.
    if (page === "Подключение")
      await screen.findByRole("region", { name: page });
    else
      await screen.findByRole("heading", {
        name: page === "Журнал" ? "Журнал и диагностика" : page,
      });
    expect(
      container.querySelector(".eyebrow, .connection-orbit, .panel .panel"),
      page,
    ).toBeNull();
    expect(container.querySelectorAll("nav svg"), page).toHaveLength(0);
  }
});

it("names the mode the same on the connection page and in settings", async () => {
  const user = userEvent.setup();
  render(
    <BackendProvider client={new MockBackend()}>
      <App />
    </BackendProvider>,
  );
  await screen.findByRole("region", { name: "Подключение" });
  const label = within(
    screen.getByText("Режим работы").parentElement as HTMLElement,
  ).getByText(/^(Proxy|TUN|TUN \+ Proxy)$/);
  await user.click(screen.getByRole("button", { name: "Настройки" }));
  const select = await screen.findByLabelText("Режим работы");
  expect(
    within(select).getByRole("option", { selected: true }),
  ).toHaveTextContent(label.textContent as string);
});

it("starts a form dialog in its first field and a confirmation on Cancel", async () => {
  const user = userEvent.setup();
  render(
    <BackendProvider client={new MockBackend()}>
      <App initialPage="servers" />
    </BackendProvider>,
  );
  await user.click(
    await screen.findByRole("button", { name: "Добавить сервер" }),
  );
  expect(screen.getByLabelText("Ссылка сервера")).toHaveFocus();
  await user.click(screen.getByRole("button", { name: "Отмена" }));
  await user.click(screen.getByRole("button", { name: "Добавить подписку" }));
  expect(screen.getByLabelText("Ссылка подписки")).toHaveFocus();
  await user.click(screen.getByRole("button", { name: "Отмена" }));
  await user.click(screen.getByRole("button", { name: "Удалить Warsaw 03" }));
  expect(
    within(screen.getByRole("dialog")).getByRole("button", { name: "Отмена" }),
  ).toHaveFocus();
});

it("keeps the connection state in the top bar on the other pages only", async () => {
  const user = userEvent.setup();
  render(
    <BackendProvider client={new MockBackend()}>
      <App />
    </BackendProvider>,
  );
  await screen.findByRole("region", { name: "Подключение" });
  expect(document.querySelector(".link-status")).toBeNull();
  await user.click(screen.getByRole("button", { name: "Серверы" }));
  expect(document.querySelector(".link-status")).toHaveTextContent(
    /^(Подключено|Отключено|Подключение…|Ошибка подключения)$/,
  );
});

it("has no remote diagnostic consent in Settings", async () => {
  render(
    <BackendProvider client={new MockBackend()}>
      <App initialPage="settings" />
    </BackendProvider>,
  );
  await screen.findByRole("heading", { name: "Настройки" });
  expect(
    screen.queryByRole("switch", { name: "Отправлять отчёты об ошибках" }),
  ).not.toBeInTheDocument();
});

it("says that the check and refresh intervals are spread by up to ten seconds", async () => {
  render(
    <BackendProvider client={new MockBackend()}>
      <App initialPage="settings" />
    </BackendProvider>,
  );
  await screen.findByRole("heading", { name: "Настройки" });
  expect(
    screen.getAllByText(
      "Фактический интервал отличается от заданного не более чем на 10 с.",
    ),
  ).toHaveLength(2);
});

it("turns starting with Windows and connecting at launch on and off from Settings", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  const execute = vi.spyOn(backend, "execute");
  render(
    <BackendProvider client={backend}>
      <App initialPage="settings" />
    </BackendProvider>,
  );
  const autostart = await screen.findByRole("switch", {
    name: "Запускать при старте",
  });
  const connect = screen.getByRole("switch", {
    name: "Подключаться при запуске",
  });
  expect(autostart).toHaveAttribute("aria-checked", "false");
  expect(connect).toHaveAttribute("aria-checked", "false");

  await user.click(autostart);
  await waitFor(() =>
    expect(autostart).toHaveAttribute("aria-checked", "true"),
  );
  expect(savedSettings(execute).at(-1)).toEqual({
    type: "save_settings",
    settings: { ...fixture.settings, autostart: true },
  });

  await user.click(connect);
  await waitFor(() => expect(connect).toHaveAttribute("aria-checked", "true"));
  expect(savedSettings(execute).at(-1)).toEqual({
    type: "save_settings",
    settings: { ...fixture.settings, autostart: true, auto_connect: true },
  });

  await user.click(autostart);
  await waitFor(() =>
    expect(autostart).toHaveAttribute("aria-checked", "false"),
  );
  expect(savedSettings(execute).at(-1)).toEqual({
    type: "save_settings",
    settings: { ...fixture.settings, auto_connect: true },
  });
});

it("turns the program lookup on from Settings and says what it costs", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  const execute = vi.spyOn(backend, "execute");
  render(
    <BackendProvider client={backend}>
      <App initialPage="settings" />
    </BackendProvider>,
  );
  const lookup = await screen.findByRole("switch", {
    name: "Показывать программу в соединениях",
  });
  expect(lookup).toHaveAttribute("aria-checked", "false");
  expect(screen.getByText(/Немного нагружает систему/)).toBeVisible();

  await user.click(lookup);
  await waitFor(() => expect(lookup).toHaveAttribute("aria-checked", "true"));
  expect(savedSettings(execute).at(-1)).toEqual({
    type: "save_settings",
    settings: { ...fixture.settings, find_process: true },
  });
});

it("returns the autostart switch to the saved value when the backend refuses it", async () => {
  const user = userEvent.setup();
  const backend = new MockBackend();
  vi.spyOn(backend, "execute").mockRejectedValue(
    "Не удалось изменить автозапуск Windows",
  );
  render(
    <BackendProvider client={backend}>
      <App initialPage="settings" />
    </BackendProvider>,
  );
  const autostart = await screen.findByRole("switch", {
    name: "Запускать при старте",
  });
  await user.click(autostart);
  await waitFor(() =>
    expect(autostart).toHaveAttribute("aria-checked", "false"),
  );
});
