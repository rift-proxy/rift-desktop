import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import {
  describe,
  ErrorBoundary,
  ErrorToasts,
  installErrorHandlers,
} from "./errors";
import { BackendProvider } from "../backend/provider";
import {
  ERROR_QUIET_MS,
  ERROR_TOAST_LIMIT,
  ERROR_TOAST_MS,
  useErrorToasts,
} from "../backend/errors";
import { MockBackend } from "../testing/mock";

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

function Harness() {
  const { toasts, dismiss } = useErrorToasts(null);
  return <ErrorToasts toasts={toasts} onDismiss={dismiss} />;
}

async function mountToasts() {
  vi.useFakeTimers();
  const backend = new MockBackend();
  render(
    <BackendProvider client={backend}>
      <Harness />
    </BackendProvider>,
  );
  // The subscription is registered asynchronously, like the host's event listener.
  await act(async () => {});
  const raise = (text: string) => act(() => backend.raiseError(text));
  const pass = (ms: number) => act(() => vi.advanceTimersByTime(ms));
  return { raise, pass };
}

it("shows a reported error as a toast that leaves by itself", async () => {
  const { raise, pass } = await mountToasts();
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  await raise("Нет доступных серверов");
  expect(screen.getByRole("alert")).toHaveTextContent("Нет доступных серверов");
  await pass(ERROR_TOAST_MS - 1);
  expect(screen.getByRole("alert")).toBeInTheDocument();
  await pass(1);
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});

it("can be dismissed early, and never opens a dialog or takes focus", async () => {
  const { raise } = await mountToasts();
  await raise("Не удалось обновить подписку");
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  expect(document.body).toHaveFocus();
  fireEvent.click(screen.getByRole("button", { name: "Скрыть ошибку" }));
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});

it("shows at most three toasts, the newest last", async () => {
  const { raise } = await mountToasts();
  for (const text of ["Первая", "Вторая", "Третья", "Четвёртая"])
    await raise(text);
  const shown = screen.getAllByRole("alert").map((toast) => toast.textContent);
  expect(shown).toHaveLength(ERROR_TOAST_LIMIT);
  expect(shown[0]).toContain("Вторая");
  expect(shown[2]).toContain("Четвёртая");
});

it("does not repeat the same text for five minutes, then shows it again", async () => {
  const { raise, pass } = await mountToasts();
  await raise("Не удалось обновить подписку");
  await pass(ERROR_TOAST_MS);
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  // A background retry fails the same way every minute: it must not blink.
  await pass(60_000);
  await raise("Не удалось обновить подписку");
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  await raise("Другая ошибка");
  expect(screen.getByRole("alert")).toHaveTextContent("Другая ошибка");
  await pass(ERROR_QUIET_MS);
  await raise("Не удалось обновить подписку");
  expect(screen.getByRole("alert")).toHaveTextContent("Не удалось обновить");
});

it("cuts a very long text for display", async () => {
  const { raise } = await mountToasts();
  await raise(`Внутренняя ошибка Rift: ${"frame ".repeat(200)}`);
  const text = screen.getByRole("alert").textContent ?? "";
  expect(text.length).toBeLessThan(450);
  expect(text).toContain("…");
});

it("reports a render crash and keeps a readable fallback", () => {
  const report = vi.fn();
  const Broken = () => {
    throw new Error("boom");
  };
  vi.spyOn(console, "error").mockImplementation(() => {});
  render(
    <ErrorBoundary logError={report}>
      <Broken />
    </ErrorBoundary>,
  );
  expect(report).toHaveBeenCalledWith(
    "Ошибка интерфейса",
    expect.stringContaining("Error: boom"),
  );
  expect(screen.getByRole("alert")).toHaveTextContent(
    "Интерфейс Rift остановился",
  );
  expect(screen.queryByText("Не удалось загрузить")).not.toBeInTheDocument();
});

it("reports uncaught errors and rejected promises", async () => {
  const report = vi.fn();
  const uninstall = installErrorHandlers(report);
  window.dispatchEvent(
    new ErrorEvent("error", {
      error: new Error("uncaught"),
      message: "uncaught",
    }),
  );
  const rejection = new Event("unhandledrejection") as Event & {
    reason?: unknown;
  };
  rejection.reason = "rejected";
  window.dispatchEvent(rejection);
  expect(report).toHaveBeenCalledWith(
    "Ошибка интерфейса",
    expect.stringContaining("Error: uncaught"),
  );
  expect(report).toHaveBeenCalledWith("Ошибка интерфейса", "rejected");
  uninstall();
});

it("contains reporter failures and non-serializable rejection reasons", async () => {
  const report = vi.fn().mockRejectedValue(new Error("host unavailable"));
  const uninstall = installErrorHandlers(report);
  const circular: { self?: unknown } = {};
  circular.self = circular;
  const rejection = new Event("unhandledrejection") as Event & {
    reason?: unknown;
  };
  rejection.reason = circular;
  window.dispatchEvent(rejection);
  await Promise.resolve();
  await Promise.resolve();
  expect(report).toHaveBeenCalledTimes(1);
  expect(report).toHaveBeenCalledWith(
    "Ошибка интерфейса",
    "[не удалось описать ошибку]",
  );
  uninstall();
});

it("shows a notice of the host as a calm toast that is no alert", async () => {
  vi.useFakeTimers();
  const backend = new MockBackend();
  render(
    <BackendProvider client={backend}>
      <Harness />
    </BackendProvider>,
  );
  await act(async () => {});
  act(() =>
    backend.raiseNotice(
      "Подтверждение Windows не получено: включён режим Proxy",
    ),
  );
  const notice = screen.getByRole("status");
  expect(notice).toHaveTextContent("Подтверждение Windows не получено");
  expect(notice).toHaveClass("info");
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  expect(
    screen.getByRole("button", { name: "Скрыть уведомление" }),
  ).toBeInTheDocument();
  expect(
    screen.queryByRole("button", { name: "Скрыть ошибку" }),
  ).not.toBeInTheDocument();
  // It leaves by itself like an error toast.
  act(() => vi.advanceTimersByTime(ERROR_TOAST_MS));
  expect(screen.queryByRole("status")).not.toBeInTheDocument();
});

it("keeps errors and notices apart, repeats of either quiet, and both under the limit", async () => {
  vi.useFakeTimers();
  const backend = new MockBackend();
  render(
    <BackendProvider client={backend}>
      <Harness />
    </BackendProvider>,
  );
  await act(async () => {});
  act(() => {
    backend.raiseError("Нет доступных серверов");
    backend.raiseNotice("Включён режим Proxy");
    backend.raiseNotice("Включён режим Proxy");
  });
  expect(screen.getAllByRole("alert")).toHaveLength(1);
  expect(screen.getAllByRole("status")).toHaveLength(1);
  act(() => {
    backend.raiseNotice("Второе уведомление");
    backend.raiseNotice("Третье уведомление");
  });
  // Three in all, the oldest pushed out.
  expect([
    ...screen.queryAllByRole("alert"),
    ...screen.queryAllByRole("status"),
  ]).toHaveLength(ERROR_TOAST_LIMIT);
  expect(screen.queryByText("Нет доступных серверов")).not.toBeInTheDocument();
  // After the quiet period the same text may be shown again.
  act(() => vi.advanceTimersByTime(ERROR_QUIET_MS));
  act(() => backend.raiseNotice("Включён режим Proxy"));
  expect(screen.getByText("Включён режим Proxy")).toBeInTheDocument();
});

it("keeps the actual error and stack as local detail rather than projecting a report", () => {
  const logError = vi.fn();
  const uninstall = installErrorHandlers(logError);
  const error = new TypeError("Cannot read property 'servers' of undefined");
  error.stack =
    "TypeError: Cannot read property 'servers' of undefined\n    at refresh (http://tauri.localhost/assets/index-ab12.js:12:34)";
  window.dispatchEvent(new ErrorEvent("error", { error }));
  expect(logError).toHaveBeenCalledWith(
    "Ошибка интерфейса",
    expect.stringContaining(error.stack),
  );
  uninstall();
});

it("keeps the browser position when an error event has no error object", () => {
  const logError = vi.fn();
  const uninstall = installErrorHandlers(logError);
  window.dispatchEvent(
    new ErrorEvent("error", {
      message: "Script error",
      filename: "http://tauri.localhost/assets/index-ab12.js",
      lineno: 12,
      colno: 34,
    }),
  );
  expect(logError).toHaveBeenCalledWith(
    "Ошибка интерфейса",
    "Script error\nhttp://tauri.localhost/assets/index-ab12.js:12:34",
  );
  uninstall();
});

it("contains hostile getters and handles a non-string stack", () => {
  const odd = new Error("odd");
  (odd as unknown as { stack: unknown }).stack = { not: "a string" };
  expect(describe(odd)).toBe("Error: odd");
  const hostile = new Error("x");
  Object.defineProperty(hostile, "stack", {
    get() {
      throw new Error("getter");
    },
  });
  expect(describe(hostile)).toBe("[не удалось описать ошибку]");
});

it("contains synchronous logger failures and removes both handlers on uninstall", () => {
  const logError = vi.fn(() => {
    throw new Error("host unavailable");
  });
  const uninstall = installErrorHandlers(logError);
  expect(() =>
    window.dispatchEvent(new ErrorEvent("error", { message: "one" })),
  ).not.toThrow();
  uninstall();
  window.dispatchEvent(new ErrorEvent("error", { message: "two" }));
  const rejection = new Event("unhandledrejection") as Event & {
    reason?: unknown;
  };
  rejection.reason = "three";
  window.dispatchEvent(rejection);
  expect(logError).toHaveBeenCalledTimes(1);
});
