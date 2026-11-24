import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { Server, ServerPreview } from "../backend/types";
import { BackendProvider } from "../backend/provider";
import { fixture, MockBackend } from "../testing/mock";
import { AddServerDialog } from "./dialogs";

afterEach(() => {
  vi.useRealTimers();
});

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (reason: unknown) => void;
}
function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
function mount(incoming?: { items: ServerPreview[]; rejected: number }) {
  vi.useFakeTimers();
  const backend = new MockBackend();
  const pending: Deferred<Server>[] = [];
  const preview = vi.spyOn(backend, "preview").mockImplementation(() => {
    const next = deferred<Server>();
    pending.push(next);
    return next.promise;
  });
  const execute = vi.fn().mockResolvedValue(true);
  const onClose = vi.fn();
  const view = render(
    <BackendProvider client={backend}>
      <AddServerDialog
        incoming={incoming}
        execute={execute}
        onClose={onClose}
      />
    </BackendProvider>,
  );
  const input = screen.queryByLabelText("Ссылка сервера");
  const change = (text: string) =>
    fireEvent.change(input!, { target: { value: text } });
  const add = screen.getByRole("button", { name: "Добавить" });
  const pass = (ms = 300) => act(() => vi.advanceTimersByTime(ms));
  return {
    ...view,
    backend,
    pending,
    preview,
    execute,
    onClose,
    change,
    add,
    pass,
  };
}
const server = (name: string): Server => ({ ...fixture.servers[0], name });

it("debounces the exact nonempty input for 300 ms and exposes a current error", async () => {
  const { change, preview, pending, add, pass } = mount();
  expect(add).toBeDisabled();
  change("   ");
  pass();
  expect(preview).not.toHaveBeenCalled();
  change(" vless://current ");
  pass(299);
  expect(preview).not.toHaveBeenCalled();
  expect(add).toBeDisabled();
  pass(1);
  expect(preview).toHaveBeenCalledWith(" vless://current ");
  expect(screen.getByRole("status")).toHaveTextContent("Распознаём ссылку…");
  await act(async () => pending[0].reject("Не удалось разобрать ссылку"));
  expect(screen.getByRole("alert")).toHaveTextContent(
    "Не удалось разобрать ссылку",
  );
  expect(add).toBeDisabled();
  change("vless://corrected");
  pass();
  expect(screen.queryByRole("alert")).toBeNull();
  await act(async () => pending[1].resolve(server("Исправленный сервер")));
  expect(
    screen.getByRole("heading", { name: "Исправленный сервер" }),
  ).toBeVisible();
  expect(add).toBeEnabled();
});

it.each(["success", "error"])(
  "ignores a late A %s after B was recognised",
  async (outcome) => {
    const { change, pending, add, execute, pass } = mount();
    change("vless://A");
    pass();
    change("vless://B");
    pass();
    await act(async () => pending[1].resolve(server("Сервер B")));
    await act(async () => {
      if (outcome === "success") pending[0].resolve(server("Сервер A"));
      else pending[0].reject("Ошибка старой ссылки A");
    });
    expect(screen.getByRole("heading", { name: "Сервер B" })).toBeVisible();
    expect(screen.queryByText("Сервер A")).toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
    fireEvent.click(add);
    expect(execute).toHaveBeenCalledExactlyOnceWith({
      type: "add_server",
      uri: "vless://B",
    });
  },
);

it.each(["success", "error"])(
  "keeps a cleared pending input empty after a late %s",
  async (outcome) => {
    const { change, pending, add, execute, pass } = mount();
    change("vless://A");
    pass();
    change("");
    expect(add).toBeDisabled();
    expect(screen.queryByRole("status")).toBeNull();
    await act(async () => {
      if (outcome === "success") pending[0].resolve(server("Сервер A"));
      else pending[0].reject("Ошибка старой ссылки A");
    });
    expect(screen.queryByText("Сервер A")).toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
    expect(add).toBeDisabled();
    fireEvent.click(add);
    expect(execute).not.toHaveBeenCalled();
  },
);

it("distinguishes generations when the user returns from A to B to A", async () => {
  const { change, pending, add, pass } = mount();
  change("vless://A");
  pass();
  change("vless://B");
  pass();
  change("vless://A");
  pass();
  await act(async () => pending[0].resolve(server("Устаревший A")));
  expect(add).toBeDisabled();
  expect(screen.queryByText("Устаревший A")).toBeNull();
  await act(async () => pending[2].resolve(server("Текущий A")));
  expect(screen.getByRole("heading", { name: "Текущий A" })).toBeVisible();
  expect(add).toBeEnabled();
  await act(async () => pending[1].reject("Ошибка B"));
  expect(screen.queryByRole("alert")).toBeNull();
});

it("invalidates a recognised link until the replacement is recognised", async () => {
  const { change, pending, add, execute, pass } = mount();
  change("vless://A");
  pass();
  await act(async () => pending[0].resolve(server("Сервер A")));
  expect(add).toBeEnabled();
  change("vless://B");
  fireEvent.click(add);
  expect(execute).not.toHaveBeenCalled();
  expect(add).toBeDisabled();
  expect(screen.queryByText("Сервер A")).toBeNull();
  pass();
  fireEvent.click(add);
  expect(execute).not.toHaveBeenCalled();
  expect(add).toBeDisabled();
  await act(async () => pending[1].resolve(server("Сервер B")));
  fireEvent.click(add);
  expect(execute).toHaveBeenCalledExactlyOnceWith({
    type: "add_server",
    uri: "vless://B",
  });
});

it("previews only the final link after fast typing stops for 300 ms", () => {
  const { change, preview, add, pass } = mount();
  for (const text of ["v", "vl", "vless://A", "vless://B"]) {
    change(text);
    pass(100);
    expect(preview).not.toHaveBeenCalled();
    expect(add).toBeDisabled();
  }
  pass(199);
  expect(preview).not.toHaveBeenCalled();
  pass(1);
  expect(preview).toHaveBeenCalledExactlyOnceWith("vless://B");
});

it.each(["clear", "unmount"])("cancels the debounce timer on %s", (action) => {
  const { change, preview, execute, unmount, pass } = mount();
  change("vless://A");
  pass(299);
  if (action === "clear") change("");
  else unmount();
  pass(1000);
  expect(preview).not.toHaveBeenCalled();
  expect(execute).not.toHaveBeenCalled();
});

it.each(["success", "error"])(
  "ignores a late A %s while B's debounce timer is pending",
  async (outcome) => {
    const { change, pending, preview, add, execute, pass } = mount();
    change("vless://A");
    pass();
    change("vless://B");
    await act(async () => {
      if (outcome === "success") pending[0].resolve(server("Сервер A"));
      else pending[0].reject("Ошибка старой ссылки A");
    });
    expect(screen.queryByText("Сервер A")).toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getByRole("status")).toHaveTextContent("Распознаём ссылку…");
    expect(add).toBeDisabled();
    fireEvent.click(add);
    expect(execute).not.toHaveBeenCalled();
    pass(299);
    expect(preview).toHaveBeenCalledTimes(1);
    pass(1);
    await act(async () => pending[1].resolve(server("Сервер B")));
    fireEvent.click(add);
    expect(execute).toHaveBeenCalledExactlyOnceWith({
      type: "add_server",
      uri: "vless://B",
    });
  },
);

it.each(["success", "error"])(
  "ignores a preview %s after unmount",
  async (outcome) => {
    const { change, pending, preview, execute, unmount, pass } = mount();
    change("vless://A");
    pass();
    unmount();
    await act(async () => {
      if (outcome === "success") pending[0].resolve(server("Сервер A"));
      else pending[0].reject("Ошибка старой ссылки A");
    });
    pass(1000);
    expect(preview).toHaveBeenCalledTimes(1);
    expect(execute).not.toHaveBeenCalled();
    expect(screen.queryByRole("dialog")).toBeNull();
  },
);

it("shows an incoming batch without requesting a preview", () => {
  const { preview, execute, add, pass } = mount({
    items: [{ uri: "vless://batch", server: server("Входящий сервер") }],
    rejected: 0,
  });
  pass(1000);
  expect(preview).not.toHaveBeenCalled();
  expect(screen.queryByLabelText("Ссылка сервера")).toBeNull();
  expect(
    screen.getByRole("heading", { name: "Входящий сервер" }),
  ).toBeVisible();
  fireEvent.click(add);
  expect(execute).toHaveBeenCalledExactlyOnceWith({
    type: "add_servers",
    uris: ["vless://batch"],
  });
});

it("cancels manual preview when an incoming batch replaces the input", () => {
  const { backend, change, preview, execute, onClose, rerender, pass } =
    mount();
  change("vless://A");
  pass(299);
  rerender(
    <BackendProvider client={backend}>
      <AddServerDialog
        incoming={{
          items: [{ uri: "vless://batch", server: server("Входящий сервер") }],
          rejected: 0,
        }}
        execute={execute}
        onClose={onClose}
      />
    </BackendProvider>,
  );
  pass(1000);
  expect(preview).not.toHaveBeenCalled();
  expect(
    screen.getByRole("heading", { name: "Входящий сервер" }),
  ).toBeVisible();
});
