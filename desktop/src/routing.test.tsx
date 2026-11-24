import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { it, expect, vi } from "vitest";
import { App } from "./App";
import { BackendProvider } from "./backend/provider";
import type { Geodata, Snapshot } from "./backend/types";
import { MockBackend, fixture } from "./testing/mock";

const mount = (state: Snapshot = fixture) => {
  const backend = new MockBackend(state);
  const execute = vi.spyOn(backend, "execute");
  render(
    <BackendProvider client={backend}>
      <App initialPage="routing" />
    </BackendProvider>,
  );
  return { backend, execute };
};
const withGeodata = (geodata: Partial<Geodata>): Snapshot => ({
  ...fixture,
  geodata: { ...fixture.geodata, ...geodata },
});
const field = (name: RegExp | string) => screen.getByRole("textbox", { name });
const saved = (execute: { mock: { calls: unknown[][] } }) =>
  execute.mock.calls
    .map(([action]) => action as { type: string })
    .filter((action) => action.type === "save_routing");

it("is the fifth page and shows the saved profile", async () => {
  mount();
  const navigation = screen.getByRole("navigation");
  expect(
    within(navigation)
      .getAllByRole("button")
      .map((button) => button.textContent),
  ).toEqual([
    "Подключение",
    "Серверы",
    "Соединения",
    "Маршрутизация",
    "Настройки",
    "Журнал",
  ]);
  expect(
    await screen.findByRole("heading", { name: "Маршрутизация" }),
  ).toBeVisible();
  expect(
    screen.getByRole("combobox", { name: "Порядок проверки" }),
  ).toHaveValue("Block → Direct → Proxy");
  // The owner's default list is there, one entry per line.
  expect(field("Правила Direct")).toHaveValue(
    fixture.routing.direct.join("\n"),
  );
  expect(field("Правила Block")).toHaveValue("");
});

it("offers exactly the six orders", async () => {
  mount();
  const select = await screen.findByRole("combobox", {
    name: "Порядок проверки",
  });
  const options = within(select)
    .getAllByRole("option")
    .map((option) => option.textContent);
  expect(options).toEqual([
    "Block → Direct → Proxy",
    "Block → Proxy → Direct",
    "Direct → Block → Proxy",
    "Direct → Proxy → Block",
    "Proxy → Block → Direct",
    "Proxy → Direct → Block",
  ]);
});

it("makes the last list of the order the rest, with no rules of its own", async () => {
  const user = userEvent.setup();
  mount();
  // Block → Direct → Proxy: Proxy is the rest.
  expect(
    await screen.findByRole("textbox", { name: "Правила Proxy" }),
  ).toBeDisabled();
  expect(field("Правила Block")).toBeEnabled();
  expect(field("Правила Direct")).toBeEnabled();
  await user.selectOptions(
    screen.getByRole("combobox", { name: "Порядок проверки" }),
    "Proxy → Direct → Block",
  );
  expect(field("Правила Block")).toBeDisabled();
  expect(field("Правила Proxy")).toBeEnabled();
  // Everything else is blocked: said in words.
  expect(screen.getByText(/Последним стоит Block/)).toBeVisible();
});

it("applies only what changed, in one action with the text of every list", async () => {
  const user = userEvent.setup();
  const { execute, backend } = mount();
  const apply = await screen.findByRole("button", { name: "Применить" });
  expect(apply).toBeDisabled();
  await user.type(
    field("Правила Block"),
    "domain:ads.example{enter}keyword:tracker",
  );
  expect(apply).toBeEnabled();
  await user.click(apply);
  await waitFor(() => expect(saved(execute)).toHaveLength(1));
  expect(saved(execute)[0]).toEqual({
    type: "save_routing",
    order: ["block", "direct", "proxy"],
    block: "domain:ads.example\nkeyword:tracker",
    direct: fixture.routing.direct.join("\n"),
    proxy: "",
  });
  // The backend has it now: the draft is the saved profile and there is nothing to apply.
  await waitFor(() => expect(apply).toBeDisabled());
  expect((await backend.snapshot()).routing.block).toEqual([
    "domain:ads.example",
    "keyword:tracker",
  ]);
});

it("keeps what was typed when the backend refuses it", async () => {
  const user = userEvent.setup();
  const { execute } = mount();
  execute.mockRejectedValue("Список Block: Не удалось разобрать «not a rule»");
  await user.type(
    await screen.findByRole("textbox", { name: "Правила Block" }),
    "not a rule",
  );
  await user.click(screen.getByRole("button", { name: "Применить" }));
  await waitFor(() => expect(execute).toHaveBeenCalled());
  expect(field("Правила Block")).toHaveValue("not a rule");
  expect(screen.getByRole("button", { name: "Применить" })).toBeEnabled();
});

it("takes back unsaved changes", async () => {
  const user = userEvent.setup();
  mount();
  await user.type(
    await screen.findByRole("textbox", { name: "Правила Block" }),
    "domain:x.example",
  );
  await user.selectOptions(
    screen.getByRole("combobox", { name: "Порядок проверки" }),
    "Direct → Proxy → Block",
  );
  await user.click(screen.getByRole("button", { name: "Отменить изменения" }));
  expect(field("Правила Block")).toHaveValue("");
  expect(
    screen.getByRole("combobox", { name: "Порядок проверки" }),
  ).toHaveValue("Block → Direct → Proxy");
  expect(screen.getByRole("button", { name: "Применить" })).toBeDisabled();
});

it("returns to the default profile only after a confirmation", async () => {
  const user = userEvent.setup();
  const custom: Snapshot = {
    ...fixture,
    routing: {
      order: ["proxy", "block", "direct"],
      block: ["domain:b.example"],
      direct: [],
      proxy: ["port:22"],
    },
  };
  const { execute } = mount(custom);
  await user.click(
    await screen.findByRole("button", { name: "Вернуть по умолчанию" }),
  );
  const dialog = await screen.findByRole("dialog");
  await user.click(within(dialog).getByRole("button", { name: "Отмена" }));
  expect(execute).not.toHaveBeenCalled();
  await user.click(
    screen.getByRole("button", { name: "Вернуть по умолчанию" }),
  );
  await user.click(
    within(await screen.findByRole("dialog")).getByRole("button", {
      name: "Вернуть",
    }),
  );
  await waitFor(() =>
    expect(execute).toHaveBeenCalledWith({ type: "reset_routing" }),
  );
  await waitFor(() =>
    expect(
      screen.getByRole("combobox", { name: "Порядок проверки" }),
    ).toHaveValue("Block → Direct → Proxy"),
  );
  expect(field("Правила Direct")).toHaveValue(
    fixture.routing.direct.join("\n"),
  );
});

it("adds a program picked in the file dialog as a process rule", async () => {
  const user = userEvent.setup();
  const { backend } = mount();
  backend.application = "Telegram.exe";
  const buttons = await screen.findAllByRole("button", {
    name: "Добавить приложение",
  });
  expect(buttons).toHaveLength(2); // the first two lists; the rest has no rules to add to
  await user.click(buttons[0]);
  await waitFor(() =>
    expect(field("Правила Block")).toHaveValue("process:Telegram.exe"),
  );
  backend.application = "chrome.exe";
  await user.click(buttons[0]);
  await waitFor(() =>
    expect(field("Правила Block")).toHaveValue(
      "process:Telegram.exe\nprocess:chrome.exe",
    ),
  );
  // A canceled dialog adds nothing.
  backend.application = null;
  await user.click(buttons[0]);
  await new Promise((resolve) => setTimeout(resolve, 20));
  expect(field("Правила Block")).toHaveValue(
    "process:Telegram.exe\nprocess:chrome.exe",
  );
});

it("shows which databases are there and what is missing", async () => {
  mount();
  expect(await screen.findByText("geosite: не загружена")).toBeVisible();
  expect(
    screen.getByText("geoip: не загружена, текущие правила её не используют"),
  ).toBeVisible();
  expect(
    screen.getByText(/Правила с geosite: и geoip: не применяются/),
  ).toBeVisible();
});

it("shows a loaded database with its date and size", async () => {
  const loaded = new Date(2026, 9, 17, 12).getTime() / 1000;
  mount(
    withGeodata({
      geosite: { updated_at: loaded, bytes: 11_205_406 },
      geoip: { updated_at: loaded, bytes: 16_587_403 },
    }),
  );
  expect(
    await screen.findByText("geosite: загружена 17.10.2026, 10,7 МБ"),
  ).toBeVisible();
  expect(
    screen.getByText("geoip: загружена 17.10.2026, 15,8 МБ"),
  ).toBeVisible();
  expect(
    screen.queryByText(/не применяются, пока базы не загружены/),
  ).toBeNull();
});

it("says when the installed version is the latest one", async () => {
  const loaded = new Date(2026, 9, 17, 12).getTime() / 1000;
  const checked = new Date(2026, 9, 19, 12).getTime() / 1000;
  mount(
    withGeodata({
      geosite: { updated_at: loaded, bytes: 11_205_406, checked_at: checked },
      geoip: { updated_at: loaded, bytes: 16_587_403 },
    }),
  );
  expect(
    await screen.findByText(
      "geosite: загружена 17.10.2026, 10,7 МБ; последняя версия, проверено 19.10.2026",
    ),
  ).toBeVisible();
  expect(
    screen.getByText("geoip: загружена 17.10.2026, 15,8 МБ"),
  ).toBeVisible();
  expect(
    screen.getByText(
      "База скачивается заново, только если на сервере версия новее.",
    ),
  ).toBeVisible();
});

it("says which database is being fetched and which waits", async () => {
  mount(withGeodata({ updating: true, current: "geoip" }));
  expect(await screen.findByText("geoip: загрузка…")).toBeVisible();
  expect(screen.getByText("geosite: ждёт очереди")).toBeVisible();
});

it("says a loaded database is being checked while it is", async () => {
  const loaded = new Date(2026, 9, 17, 12).getTime() / 1000;
  mount(
    withGeodata({
      geosite: { updated_at: loaded, bytes: 11_205_406 },
      updating: true,
      current: "geosite",
    }),
  );
  expect(await screen.findByText("geosite: проверка версии…")).toBeVisible();
});

it("asks for the database the profile uses, whatever the profile says about the other", async () => {
  mount(withGeodata({ uses_geosite: false, uses_geoip: true }));
  expect(await screen.findByText("geoip: не загружена")).toBeVisible();
  expect(
    screen.getByText("geosite: не загружена, текущие правила её не используют"),
  ).toBeVisible();
});

it("shows a download in progress and keeps the update from starting a second one", async () => {
  mount(withGeodata({ updating: true }));
  expect(await screen.findByText("geosite: загрузка…")).toBeVisible();
  expect(screen.getByRole("button", { name: "Обновить базы" })).toBeDisabled();
});

it("says why a download failed", async () => {
  mount(withGeodata({ failure_code: "geodata_download" }));
  expect(await screen.findByRole("alert")).toHaveTextContent(
    "Не удалось загрузить базу маршрутизации",
  );
});

it("disables the update where the build cannot download", async () => {
  mount(withGeodata({ available: false }));
  expect(
    await screen.findByRole("button", { name: "Обновить базы" }),
  ).toBeDisabled();
  expect(
    screen.getByText("В этой сборке загрузка баз недоступна."),
  ).toBeVisible();
});

it("updates the databases on request", async () => {
  const user = userEvent.setup();
  const { execute } = mount();
  await user.click(
    await screen.findByRole("button", { name: "Обновить базы" }),
  );
  expect(execute).toHaveBeenCalledWith({ type: "update_geodata" });
  expect(await screen.findByText(/geosite: загружена/)).toBeVisible();
});

it("takes a profile imported while the page is open, unless it has unsaved edits", async () => {
  const user = userEvent.setup();
  const { backend } = mount();
  await screen.findByRole("heading", { name: "Маршрутизация" });
  // Untouched: the imported profile appears.
  await act(async () => {
    await backend.execute({
      type: "save_routing",
      order: ["direct", "proxy", "block"],
      block: "",
      direct: "domain:imported.example",
      proxy: "",
    });
  });
  await waitFor(() =>
    expect(
      screen.getByRole("combobox", { name: "Порядок проверки" }),
    ).toHaveValue("Direct → Proxy → Block"),
  );
  expect(field("Правила Direct")).toHaveValue("domain:imported.example");
  // Edited: what is typed stays.
  await user.type(field("Правила Proxy"), "port:22");
  await act(async () => {
    await backend.execute({
      type: "save_routing",
      order: ["direct", "proxy", "block"],
      block: "",
      direct: "domain:other.example",
      proxy: "",
    });
  });
  expect(field("Правила Proxy")).toHaveValue("port:22");
});

it("explains the syntax", async () => {
  const user = userEvent.setup();
  mount();
  await user.click(await screen.findByText("Какие записи понимает Rift"));
  expect(screen.getByText("geosite:category-ru")).toBeVisible();
  expect(screen.getByText("process:chrome.exe")).toBeVisible();
  expect(screen.getByText("udp:443, tcp:1000-2000")).toBeVisible();
  expect(screen.getByText("protocol:bittorrent")).toBeVisible();
});
