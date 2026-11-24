import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { it, expect, vi } from "vitest";
import { App } from "./App";
import { BackendProvider } from "./backend/provider";
import type { Entry, Snapshot } from "./backend/types";
import { MockBackend, fixture } from "./testing/mock";

const entry = (
  at: number,
  category: Entry["category"],
  message: string,
  level = "info",
): Entry => ({ at, level, message, category });
const journal: Entry[] = [
  entry(1, "core", "sing-box запущен"),
  entry(2, "checks", "Проверка серверов запущена"),
  entry(3, "checks", "Проверка завершена: проверено 6, достижимо 3, рабочих 3"),
  entry(4, "subscriptions", "Подписка обновлена; отклонено строк: 0"),
  entry(
    5,
    "subscriptions",
    "Не удалось обновить подписку; предыдущие серверы сохранены",
    "error",
  ),
  entry(6, "app", "Серверов добавлено: 2"),
  entry(7, "core", "Обнаружено завершение sing-box", "error"),
];
const mount = (state: Snapshot = { ...fixture, journal }) => {
  const backend = new MockBackend(state);
  const execute = vi.spyOn(backend, "execute");
  render(
    <BackendProvider client={backend}>
      <App initialPage="journal" />
    </BackendProvider>,
  );
  return { backend, execute };
};
/** The messages of the rows now in the journal table (not the measurements above it). */
const messages = () =>
  within(
    screen.getByRole("columnheader", { name: "Сообщение" }).closest("table")!,
  )
    .getAllByRole("row")
    .slice(1)
    .map((row) => within(row).getAllByRole("cell").at(-1)!.textContent);

it("shows every entry with its source on the first tab", async () => {
  mount();
  await screen.findByRole("heading", { name: "Журнал и диагностика" });
  expect(messages()).toHaveLength(7);
  expect(screen.getByRole("columnheader", { name: "Источник" })).toBeVisible();
  const row = screen
    .getByText("Подписка обновлена; отклонено строк: 0")
    .closest("tr")!;
  expect(within(row).getByText("Подписки")).toBeVisible();
  expect(screen.getByRole("button", { name: "Все" })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
});

it("shows the date with the time of every entry, not the time alone", async () => {
  const stamp = new Date(2026, 9, 21, 14, 3, 7).getTime() / 1000;
  mount({
    ...fixture,
    journal: [entry(stamp, "app", "Серверов добавлено: 2")],
  });
  await screen.findByRole("heading", { name: "Журнал и диагностика" });
  expect(
    screen.getByRole("columnheader", { name: "Дата и время" }),
  ).toBeVisible();
  const row = screen.getByText("Серверов добавлено: 2").closest("tr")!;
  expect(within(row).getAllByRole("cell")[0]).toHaveTextContent(
    /^21\.10\.2026 14:03:07$/,
  );
});

it.each([
  ["Приложение", ["Серверов добавлено: 2"]],
  [
    "Подписки",
    [
      "Подписка обновлена; отклонено строк: 0",
      "Не удалось обновить подписку; предыдущие серверы сохранены",
    ],
  ],
  [
    "Проверки серверов",
    [
      "Проверка серверов запущена",
      "Проверка завершена: проверено 6, достижимо 3, рабочих 3",
    ],
  ],
  ["Ядро", ["sing-box запущен", "Обнаружено завершение sing-box"]],
])("the %s tab shows only its own entries", async (tab, expected) => {
  const user = userEvent.setup();
  mount();
  await user.click(await screen.findByRole("button", { name: tab }));
  expect(messages()).toEqual(expected);
  // The source is the tab now: no column for it.
  expect(screen.queryByRole("columnheader", { name: "Источник" })).toBeNull();
  expect(screen.getByRole("button", { name: tab })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
});

it("combines the source with the level and the search", async () => {
  const user = userEvent.setup();
  mount();
  await user.click(await screen.findByRole("button", { name: "Подписки" }));
  await user.selectOptions(screen.getByLabelText("Уровень"), "error");
  expect(messages()).toEqual([
    "Не удалось обновить подписку; предыдущие серверы сохранены",
  ]);
  await user.selectOptions(screen.getByLabelText("Уровень"), "all");
  await user.type(screen.getByLabelText("Поиск в журнале"), "ОБНОВЛЕНА");
  expect(messages()).toEqual(["Подписка обновлена; отклонено строк: 0"]);
  // Back to all sources: the search still applies across them.
  await user.click(screen.getByRole("button", { name: "Все" }));
  expect(messages()).toEqual(["Подписка обновлена; отклонено строк: 0"]);
});

it("says so when a source has nothing, and differs from an empty journal", async () => {
  const user = userEvent.setup();
  mount({ ...fixture, journal: journal.filter((e) => e.category !== "core") });
  await user.click(await screen.findByRole("button", { name: "Ядро" }));
  expect(screen.getByText("Ничего не найдено")).toBeVisible();
  expect(screen.queryByText("В журнале пока тихо")).toBeNull();
});

it("an empty journal is quiet on every tab", async () => {
  const user = userEvent.setup();
  mount({ ...fixture, journal: [] });
  expect(await screen.findByText("В журнале пока тихо")).toBeVisible();
  await user.click(screen.getByRole("button", { name: "Ядро" }));
  expect(screen.getByText("В журнале пока тихо")).toBeVisible();
});

it("clears all sources and exports only after the privacy confirmation", async () => {
  const user = userEvent.setup();
  const { execute } = mount();
  await user.click(await screen.findByRole("button", { name: "Ядро" }));
  await user.click(screen.getByRole("button", { name: "Очистить" }));
  await user.click(screen.getByRole("button", { name: "Экспорт диагностики" }));
  expect(execute.mock.calls.map(([action]) => action.type)).toEqual([
    "clear_journal",
  ]);
  const dialog = screen.getByRole("dialog");
  expect(dialog).toHaveTextContent("Rift ничего не отправляет автоматически");
  await user.click(
    within(dialog).getByRole("button", { name: "Сохранить файл" }),
  );
  expect(execute.mock.calls.map(([action]) => action.type)).toEqual([
    "clear_journal",
    "export_diagnostics",
  ]);
});

it("canceling export never asks Rust to write a file and restores focus", async () => {
  const user = userEvent.setup();
  const { execute } = mount();
  const button = await screen.findByRole("button", {
    name: "Экспорт диагностики",
  });
  await user.click(button);
  const dialog = screen.getByRole("dialog");
  expect(within(dialog).getByRole("button", { name: "Отмена" })).toHaveFocus();
  await user.click(within(dialog).getByRole("button", { name: "Отмена" }));
  expect(execute).not.toHaveBeenCalled();
  expect(button).toHaveFocus();
});

it("reveals local details on demand and finds technical fields through search", async () => {
  const user = userEvent.setup();
  const detail =
    "Xray parser: missing field settings.vnext[0].users at line 4 column 8";
  mount({
    ...fixture,
    journal: [
      {
        ...entry(
          1,
          "subscriptions",
          "Подписка содержит некорректные данные",
          "error",
        ),
        details: detail,
      },
    ],
  });
  const summary = await screen.findByText("Технические подробности");
  const disclosure = summary.closest("details")!;
  expect(disclosure).not.toHaveAttribute("open");
  await user.click(summary);
  expect(screen.getByText(detail)).toBeVisible();
  await user.type(screen.getByLabelText("Поиск в журнале"), "VNEXT");
  expect(
    screen.getByText("Подписка содержит некорректные данные"),
  ).toBeVisible();
});

it("does not transfer disclosure across filters, appended snapshots or rotation", async () => {
  const user = userEvent.setup();
  const first: Entry = {
    ...entry(10, "app", "Original application failure", "error"),
    details: "assets/index-ab12.js:12:34",
  };
  const second: Entry = {
    ...entry(11, "subscriptions", "Different subscription failure", "error"),
    details: "field=settings.vnext",
  };
  const { backend } = mount({ ...fixture, journal: [first, second] });
  const disclosure = (message: string) =>
    screen.getByText(message).closest("tr")!.querySelector("details")!;
  await user.click(
    within((await screen.findByText(first.message)).closest("tr")!).getByText(
      "Технические подробности",
    ),
  );
  expect(disclosure(first.message)).toHaveAttribute("open");
  await user.click(
    screen.getByRole("button", { name: "Подписки" }),
  );
  expect(disclosure(second.message)).not.toHaveAttribute("open");
  await user.click(screen.getByRole("button", { name: "Все" }));
  await user.click(
    within(screen.getByText(first.message).closest("tr")!).getByText(
      "Технические подробности",
    ),
  );
  const third: Entry = {
    ...entry(12, "app", "New failure", "error"),
    details: "stage=append",
  };
  await act(async () =>
    backend.update((state) => {
      state.journal = [first, second, third];
    }),
  );
  expect(disclosure(first.message)).toHaveAttribute("open");
  expect(disclosure(third.message)).not.toHaveAttribute("open");
  await act(async () =>
    backend.update((state) => {
      state.journal = [second, third];
    }),
  );
  expect(disclosure(second.message)).not.toHaveAttribute("open");
  expect(disclosure(third.message)).not.toHaveAttribute("open");
});

it("marks a warning and filters by it, apart from errors and information", async () => {
  const user = userEvent.setup();
  mount({
    ...fixture,
    journal: [
      ...journal,
      entry(
        8,
        "checks",
        "Соединение потеряно: выбранный сервер не отвечает",
        "warn",
      ),
    ],
  });
  const row = (
    await screen.findByText("Соединение потеряно: выбранный сервер не отвечает")
  ).closest("tr")!;
  expect(within(row).getByText("Внимание")).toBeVisible();
  await user.selectOptions(screen.getByLabelText("Уровень"), "warn");
  expect(messages()).toEqual([
    "Соединение потеряно: выбранный сервер не отвечает",
  ]);
  await user.selectOptions(screen.getByLabelText("Уровень"), "error");
  expect(messages()).not.toContain(
    "Соединение потеряно: выбранный сервер не отвечает",
  );
});
