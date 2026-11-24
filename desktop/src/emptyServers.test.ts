import { describe, expect, it } from "vitest";
import { emptyServers } from "./emptyServers";
import type { Snapshot, Subscription } from "./backend/types";
import { fixture } from "./testing/mock";

const subscription = (changes: Partial<Subscription> = {}): Subscription => ({
  ...fixture.subscriptions[0],
  id: "s",
  name: "Основная",
  count: 0,
  updated_at: 1789742400,
  error: null,
  failure_code: null,
  ...changes,
});
const state = (
  subscriptions: Subscription[],
  changes: Partial<Snapshot> = {},
): Pick<Snapshot, "servers" | "subscriptions" | "refreshing"> => ({
  servers: [],
  subscriptions,
  refreshing: false,
  ...changes,
});

describe("what the list of servers says when it is empty", () => {
  it("says it is loading while a refresh is under way", () => {
    expect(
      emptyServers(state([subscription()], { refreshing: true }), "s"),
    ).toEqual({
      title: "Загружаем серверы…",
      description: "",
      action: null,
    });
  });

  it.each([
    ["subscription_transport", "Не удалось загрузить подписку"],
    ["subscription_timeout", "Загрузка подписки заняла слишком много времени"],
    ["subscription_http_status", "Сервис подписки вернул ошибку"],
  ])(
    "gives the reason of a failed download (%s) and offers to try again, not to add a server",
    (code, reason) => {
      const result = emptyServers(
        state([subscription({ failure_code: code as never, error: "x" })]),
        "s",
      );
      expect(result.title).toBe(
        "Не удалось получить серверы подписки «Основная»",
      );
      expect(result.description).toBe(reason);
      expect(result.action).toEqual({
        label: "Обновить",
        kind: "refresh",
        id: "s",
      });
    },
  );

  it("says a body without supported servers is that, not a failed download", () => {
    const result = emptyServers(
      state([
        subscription({ failure_code: "subscription_invalid_body" as never }),
      ]),
      "s",
    );
    expect(result.title).toBe("Подписка не содержит серверов");
    expect(result.description).toBe("Подписка содержит некорректные данные");
    expect(result.action).toEqual({
      label: "Обновить",
      kind: "refresh",
      id: "s",
    });
  });

  it("says the filters hid everything and offers to change them", () => {
    const result = emptyServers(
      state([subscription({ failure_code: "subscription_filtered" as never })]),
      "s",
    );
    expect(result.title).toBe("Фильтры скрыли все серверы подписки");
    expect(result.description).toBe("Измените фильтры подписки");
    expect(result.action).toEqual({
      label: "Изменить подписку",
      kind: "edit",
      id: "s",
    });
  });

  it("says a subscription that was never loaded is not loaded yet", () => {
    const result = emptyServers(
      state([subscription({ updated_at: null })]),
      "s",
    );
    expect(result.title).toBe("Серверы подписки ещё не загружены");
    expect(result.action).toEqual({
      label: "Обновить",
      kind: "refresh",
      id: "s",
    });
  });

  it("keeps the old words for the manual list and for a list with nothing at all", () => {
    const others = state([subscription()], { servers: [fixture.servers[0]] });
    expect(emptyServers(others, "manual")).toEqual({
      title: "В этом источнике пока нет серверов",
      description: "Выберите другой источник слева или добавьте сервер",
      action: { label: "Добавить сервер", kind: "add_server" },
    });
    expect(emptyServers(state([]), "manual")).toEqual({
      title: "Серверов пока нет",
      description: "Добавьте сервер вручную.",
      action: { label: "Добавить сервер", kind: "add_server" },
    });
    expect(emptyServers(state([]), "all")).toEqual({
      title: "Серверов пока нет",
      description: "Добавьте ссылку подписки или сервер вручную.",
      action: { label: "Добавить подписку", kind: "add_subscription" },
    });
  });

  it("says that no subscription loaded when there are subscriptions and no servers at all", () => {
    const result = emptyServers(
      state([
        subscription({
          failure_code: "subscription_transport" as never,
          error: "x",
        }),
      ]),
      "all",
    );
    expect(result.title).toBe("Серверов пока нет: подписки не загрузились");
    expect(result.description).toBe("Не удалось загрузить подписку");
    expect(result.action).toEqual({
      label: "Обновить все",
      kind: "refresh_all",
    });
    // Nothing failed yet, only nothing came: the same title with the way out.
    expect(emptyServers(state([subscription()]), "all").action?.kind).toBe(
      "refresh_all",
    );
  });
});
