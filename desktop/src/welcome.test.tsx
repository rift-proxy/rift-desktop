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
import { App, type Page } from "./App";
import { BackendProvider } from "./backend/provider";
import type { Snapshot } from "./backend/types";
import { MockBackend, fixture } from "./testing/mock";

const firstRun: Snapshot = {
  ...fixture,
  servers: [],
  subscriptions: [],
  selected: null,
  status: "disconnected",
  runtime_active: false,
  welcome: true,
};
const mount = async (state: Snapshot = firstRun, page: Page = "connection") => {
  const backend = new MockBackend(state);
  const execute = vi.spyOn(backend, "execute");
  await act(async () => {
    render(
      <BackendProvider client={backend}>
        <App initialPage={page} />
      </BackendProvider>,
    );
  });
  return { backend, execute };
};
const actions = (execute: { mock: { calls: unknown[][] } }) =>
  execute.mock.calls.map(([action]) => action as { type: string });

it("welcomes a first start with one field and a way out", async () => {
  await mount();
  const dialog = screen.getByRole("dialog");
  expect(within(dialog).getByText("Добро пожаловать в Rift")).toBeVisible();
  expect(
    within(dialog).getByLabelText("Ссылка подписки или сервера"),
  ).toHaveFocus();
  expect(
    within(dialog).getByRole("button", { name: "Пропустить" }),
  ).toBeEnabled();
  expect(
    within(dialog).getByRole("button", { name: "Добавить" }),
  ).toBeDisabled();
  expect(
    within(dialog).queryByRole("button", { name: "Закрыть диалог" }),
  ).toBeNull();
});

it("is not shown when the backend does not ask for it", async () => {
  await mount({ ...firstRun, welcome: false });
  await screen.findByRole("button", { name: "Подключиться" });
  expect(screen.queryByRole("dialog")).toBeNull();
});

it("recognises a subscription link, adds it on confirm and goes away", async () => {
  const user = userEvent.setup();
  const { execute, backend } = await mount();
  const dialog = await screen.findByRole("dialog");
  await user.type(
    within(dialog).getByLabelText("Ссылка подписки или сервера"),
    "https://panel.example/sub?token=abc",
  );
  expect(
    await within(dialog).findByText("Подписка: panel.example"),
  ).toBeVisible();
  expect(actions(execute)).toEqual([]);
  await user.click(within(dialog).getByRole("button", { name: "Добавить" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(execute).toHaveBeenCalledWith({
    type: "save_subscription",
    id: null,
    url: "https://panel.example/sub?token=abc",
    interval_mins: null,
    user_agent: null,
    filters: { include: [], exclude: [], protocols: [] },
    provider_routes: true,
  });
  expect((await backend.snapshot()).welcome).toBe(false);
  // The connection does not start by itself.
  expect(actions(execute).map((action) => action.type)).not.toContain(
    "connect",
  );
});

it("recognises server links and adds them all", async () => {
  const user = userEvent.setup();
  const { execute } = await mount();
  const dialog = await screen.findByRole("dialog");
  await user.click(
    within(dialog).getByLabelText("Ссылка подписки или сервера"),
  );
  await user.paste("vless://u@a.example:443 vless://u@b.example:443");
  expect(await within(dialog).findByText("Найдено серверов: 2")).toBeVisible();
  await user.click(within(dialog).getByRole("button", { name: "Добавить" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(execute).toHaveBeenCalledWith({
    type: "add_servers",
    uris: ["vless://u@a.example:443", "vless://u@b.example:443"],
  });
});

it("refuses text without a link and adds nothing", async () => {
  const user = userEvent.setup();
  const { execute } = await mount();
  const dialog = await screen.findByRole("dialog");
  await user.type(
    within(dialog).getByLabelText("Ссылка подписки или сервера"),
    "это просто слова",
  );
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "В тексте нет ссылки подписки или сервера",
  );
  expect(
    within(dialog).getByRole("button", { name: "Добавить" }),
  ).toBeDisabled();
  expect(actions(execute)).toEqual([]);
});

it("takes Skip and Escape as the answer: it is not asked again", async () => {
  const user = userEvent.setup();
  const first = await mount();
  await user.click(
    within(await screen.findByRole("dialog")).getByRole("button", {
      name: "Пропустить",
    }),
  );
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(actions(first.execute)).toEqual([{ type: "dismiss_welcome" }]);
  expect((await first.backend.snapshot()).welcome).toBe(false);
});

it("treats Escape like Skip", async () => {
  const { execute } = await mount();
  const dialog = await screen.findByRole("dialog");
  // Escape reaches the dialog as `cancel`.
  fireEvent(dialog, new Event("cancel", { cancelable: true }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(actions(execute)).toEqual([{ type: "dismiss_welcome" }]);
});

it("stays open when adding fails, so nothing is lost", async () => {
  const user = userEvent.setup();
  const { execute } = await mount();
  execute.mockRejectedValue("Не удалось сохранить подписку");
  const dialog = await screen.findByRole("dialog");
  await user.type(
    within(dialog).getByLabelText("Ссылка подписки или сервера"),
    "https://panel.example/sub",
  );
  await within(dialog).findByText("Подписка: panel.example");
  await user.click(within(dialog).getByRole("button", { name: "Добавить" }));
  expect(await within(dialog).findByRole("alert")).toHaveTextContent(
    "Не удалось сохранить подписку",
  );
  expect(screen.getByRole("dialog")).toBeVisible();
  expect(
    within(dialog).getByLabelText("Ссылка подписки или сервера"),
  ).toHaveValue("https://panel.example/sub");
});

it("does not take a paste on the page of servers while the welcome is open", async () => {
  await mount(firstRun, "servers");
  await screen.findByRole("dialog");
  document.dispatchEvent(
    Object.assign(new Event("paste", { bubbles: true, cancelable: true }), {
      clipboardData: { getData: () => "vless://u@a.example:443" },
    }),
  );
  await new Promise((resolve) => setTimeout(resolve, 50));
  expect(screen.getAllByRole("dialog")).toHaveLength(1);
  expect(screen.getByRole("dialog")).toHaveTextContent(
    "Добро пожаловать в Rift",
  );
  expect(screen.getByLabelText("Ссылка подписки или сервера")).toHaveValue("");
});

it("cannot submit the old preview while changed input waits for recognition", async () => {
  const user = userEvent.setup();
  const { execute } = await mount();
  const input = await screen.findByLabelText("Ссылка подписки или сервера");
  await user.type(input, "https://first.example/sub");
  await screen.findByText("Подписка: first.example");
  fireEvent.change(input, { target: { value: "https://second.example/sub" } });
  expect(screen.getByRole("button", { name: "Добавить" })).toBeDisabled();
  fireEvent.submit(input.closest("form")!);
  expect(actions(execute)).toEqual([]);
  await screen.findByText("Подписка: second.example");
  expect(screen.getByRole("button", { name: "Добавить" })).toBeEnabled();
});
