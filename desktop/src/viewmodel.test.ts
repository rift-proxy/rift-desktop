import { describe, expect, it } from "vitest";
import {
  DEFAULT_SORT,
  nextSort,
  settingsProblems,
  sortServers,
  visibleServers,
} from "./viewmodel";
import type {
  Server,
  ServersSort,
  Settings,
  SortColumn,
} from "./backend/types";
import { fixture } from "./testing/mock";
const show = (source: string) =>
  visibleServers(fixture.servers, source, fixture.subscriptions, DEFAULT_SORT);
describe("server presentation", () => {
  it("filters by source without changing backend order", () => {
    const before = fixture.servers.map((s) => s.id);
    expect(show("backup").map((s) => s.id)).toEqual(["c"]);
    expect(show("manual").map((s) => s.id)).toEqual(["d"]);
    expect(fixture.servers.map((s) => s.id)).toEqual(before);
  });
  it("sorts by name, all source", () => {
    expect(show("all").map((s) => s.id)).toEqual(["c", "a", "b", "d"]);
  });
});

describe("settings that may be sent to the backend", () => {
  const base = fixture.settings;
  const withValue = (changes: Partial<Settings>): Settings => ({
    ...base,
    ...changes,
  });

  it("accepts the settings the core accepts by default", () => {
    expect(settingsProblems(base)).toEqual({});
  });

  it.each([
    ["proxy_port", 1, 65535, { api_port: 2081 }],
    ["api_port", 1, 65535, { proxy_port: 2080 }],
    ["interval_secs", 50, 300, {}],
    ["pool_size", 1, 5, {}],
    ["full_interval_mins", 10, 30, {}],
    ["degrade_percent", 10, 300, {}],
    ["degrade_cap_ms", 200, 10000, {}],
    ["endpoint_timeout_ms", 100, 30000, {}],
    ["proxy_timeout_ms", 100, 30000, {}],
    ["concurrency", 1, 3, {}],
    ["subscription_interval_mins", 1, 10080, {}],
    ["switch_threshold_percent", 0, 100, {}],
    ["switch_threshold_ms", 0, 10000, {}],
  ] as const)(
    "keeps %s between %i and %i, whole numbers only",
    (key, min, max, others) => {
      const message = `От ${min} до ${max}`;
      const only = (value: number) =>
        settingsProblems(withValue({ ...others, [key]: value }));
      expect(only(min)).toEqual({});
      expect(only(max)).toEqual({});
      expect(only(min - 1)).toEqual({ [key]: message });
      expect(only(max + 1)).toEqual({ [key]: message });
      expect(only(min + 0.5)).toEqual({ [key]: message });
      expect(only(Number.NaN)).toEqual({ [key]: message });
    },
  );

  it("reports each wrong field and no other", () => {
    expect(
      settingsProblems(withValue({ proxy_port: 0, concurrency: 4 })),
    ).toEqual({
      proxy_port: "От 1 до 65535",
      concurrency: "От 1 до 3",
    });
  });

  it("does not let the API use the port of the proxy", () => {
    expect(settingsProblems(withValue({ api_port: base.proxy_port }))).toEqual({
      api_port: "Порт API должен отличаться от порта прокси",
    });
  });

  it("names the range, not the clash, when both ports are the same wrong number", () => {
    expect(settingsProblems(withValue({ proxy_port: 0, api_port: 0 }))).toEqual(
      { proxy_port: "От 1 до 65535", api_port: "От 1 до 65535" },
    );
  });

  it.each([
    "https://example.com/generate_204",
    "https://example.com:8443/path?x=1",
  ])("accepts the https address %s", (connectivity_url) => {
    expect(settingsProblems(withValue({ connectivity_url }))).toEqual({});
  });

  it.each([
    "http://example.com/generate_204",
    "ftp://example.com/",
    "https://user@example.com/",
    "https://:secret@example.com/",
    "https://user:secret@example.com/",
    "example.com",
    "https://",
    "",
  ])("refuses the address %j", (connectivity_url) => {
    expect(settingsProblems(withValue({ connectivity_url }))).toEqual({
      connectivity_url: "Нужен адрес https:// без логина и пароля",
    });
  });
});

const server = (id: string, over: Partial<Server> = {}): Server => ({
  id,
  name: id,
  protocol: "vless",
  source: null,
  endpoint_ms: null,
  proxy_ms: null,
  status: "unchecked",
  checked_at: null,
  failure_code: null,
  ...over,
});
const ids = (list: Server[]) => list.map((s) => s.id);
const by = (column: SortColumn, descending = false): ServersSort => ({
  column,
  descending,
});

describe("ordering the servers", () => {
  const list = [
    server("b", { protocol: "trojan", endpoint_ms: 40, proxy_ms: 90 }),
    server("a", { protocol: "vless", endpoint_ms: 30, proxy_ms: 120 }),
    server("c", { protocol: "shadowsocks", endpoint_ms: 50, proxy_ms: 60 }),
  ];
  it.each([
    ["name", ["a", "b", "c"]],
    ["protocol", ["c", "b", "a"]],
    ["endpoint_ms", ["a", "b", "c"]],
    ["proxy_ms", ["c", "b", "a"]],
  ] as [SortColumn, string[]][])(
    "orders by %s upwards and downwards",
    (column, upwards) => {
      expect(ids(sortServers(list, [], by(column)))).toEqual(upwards);
      expect(ids(sortServers(list, [], by(column, true)))).toEqual(
        [...upwards].reverse(),
      );
    },
  );

  it("puts the servers kept out of Auto Select last, whatever the column, direction and value", () => {
    const mixed = [
      server("kept-fast", { proxy_ms: 5, excluded: true, status: "online" }),
      server("slow", { proxy_ms: 300, status: "failed" }),
      server("fast", { proxy_ms: 20, status: "online" }),
      server("kept-none", { excluded: true }),
      server("none"),
    ];
    const columns: SortColumn[] = [
      "name",
      "protocol",
      "endpoint_ms",
      "proxy_ms",
      "status",
      "source",
      "checked_at",
    ];
    for (const column of columns) {
      for (const descending of [false, true]) {
        const order = ids(sortServers(mixed, [], by(column, descending)));
        const kept = new Set(["kept-fast", "kept-none"]);
        const firstKept = order.findIndex((id) => kept.has(id));
        expect(firstKept, `${column} ${descending}`).toBe(3);
        expect(order.slice(3).every((id) => kept.has(id))).toBe(true);
      }
    }
    // Inside each group the column still orders: the fast one among the kept comes first.
    expect(ids(sortServers(mixed, [], by("proxy_ms")))).toEqual([
      "fast",
      "slow",
      "none",
      "kept-fast",
      "kept-none",
    ]);
  });

  it("orders names by the Russian collation, not by code point", () => {
    const names = ["Яндекс", "Берлин", "amsterdam", "Zürich", "Вена"];
    const sorted = sortServers(
      names.map((name) => server(name, { name })),
      [],
      by("name"),
    );
    // Russian collation: Cyrillic before Latin, case and the umlaut do not decide.
    expect(sorted.map((s) => s.name)).toEqual([
      "Берлин",
      "Вена",
      "Яндекс",
      "amsterdam",
      "Zürich",
    ]);
  });

  it("puts a server without a measurement last in either direction", () => {
    const mixed = [
      server("none"),
      server("slow", { proxy_ms: 200, checked_at: 20 }),
      server("fast", { proxy_ms: 10, checked_at: 10 }),
      server("zero", { proxy_ms: 0, checked_at: 0 }),
    ];
    for (const column of ["proxy_ms", "checked_at"] as const) {
      const up = ids(sortServers(mixed, [], by(column)));
      const down = ids(sortServers(mixed, [], by(column, true)));
      expect(up.at(-1), column).toBe("none");
      expect(down.at(-1), column).toBe("none");
    }
    expect(ids(sortServers(mixed, [], by("proxy_ms")))).toEqual([
      "zero",
      "fast",
      "slow",
      "none",
    ]);
    expect(ids(sortServers(mixed, [], by("proxy_ms", true)))).toEqual([
      "slow",
      "fast",
      "zero",
      "none",
    ]);
  });

  it("orders statuses from the one in use to the unreachable, unknown last", () => {
    const states = [
      "unreachable",
      "failed",
      "mystery",
      "online",
      "unchecked",
      "reachable",
    ];
    const mixed = states.map((status) => server(status, { status }));
    expect(ids(sortServers(mixed, [], by("status")))).toEqual([
      "online",
      "reachable",
      "unchecked",
      "failed",
      "unreachable",
      "mystery",
    ]);
  });

  it("puts the server in use first by status, however its last check went", () => {
    const list = [
      server("x", { status: "online", name: "A" }),
      server("y", { status: "failed", name: "B" }),
      server("z", { status: "online", name: "C" }),
    ];
    expect(ids(sortServers(list, [], by("status"), "y"))).toEqual([
      "y",
      "x",
      "z",
    ]);
    // Backwards it is last, like the best of any order; without one in use nothing is lifted.
    expect(ids(sortServers(list, [], by("status", true), "y"))).toEqual([
      "x",
      "z",
      "y",
    ]);
    expect(ids(sortServers(list, [], by("status")))).toEqual(["x", "z", "y"]);
    expect(
      ids(
        visibleServers(
          list as typeof fixture.servers,
          "all",
          [],
          by("status"),
          "z",
        ),
      ),
    ).toEqual(["z", "x", "y"]);
    // Another column does not lift it.
    expect(ids(sortServers(list, [], by("name"), "z"))).toEqual([
      "x",
      "y",
      "z",
    ]);
  });

  it("orders by the name of the source, the manual ones as «Вручную»", () => {
    const sources = [
      { id: "s1", name: "Яблоко" },
      { id: "s2", name: "Арбуз" },
    ] as Parameters<typeof sortServers>[1];
    const mixed = [
      server("p", { source: "s1" }),
      server("q", { source: "s2" }),
      server("r", { source: null }),
      server("lost", { source: "gone" }),
    ];
    expect(ids(sortServers(mixed, sources, by("source")))).toEqual([
      "q",
      "lost",
      "r",
      "p",
    ]);
  });

  it("keeps equal values in the order of the names, whichever way", () => {
    // The ids run against the names, so an order by id would show.
    const equal = [
      server("1", { name: "Charlie", proxy_ms: 5 }),
      server("3", { name: "Alpha", proxy_ms: 5 }),
      server("2", { name: "Bravo", proxy_ms: 5 }),
      server("n2", { name: "Nil" }),
      server("n1", { name: "Echo" }),
    ];
    const expected = ["3", "2", "1", "n1", "n2"];
    expect(ids(sortServers(equal, [], by("proxy_ms")))).toEqual(expected);
    expect(ids(sortServers(equal, [], by("proxy_ms", true)))).toEqual(expected);
    expect(ids(sortServers([...equal].reverse(), [], by("proxy_ms")))).toEqual(
      expected,
    );
  });

  it("falls back to the id when the names are the same, and leaves the input alone", () => {
    const same = [server("2", { name: "Same" }), server("1", { name: "Same" })];
    const before = ids(same);
    expect(ids(sortServers(same, [], by("name")))).toEqual(["1", "2"]);
    expect(ids(sortServers([...same].reverse(), [], by("name")))).toEqual([
      "1",
      "2",
    ]);
    expect(ids(same)).toEqual(before);
  });

  it("keeps the order it was given for copies of one server, whichever way", () => {
    // The same server can come from two subscriptions: same id, same name.
    const copies = [
      server("x", { name: "Copy", source: "one" }),
      server("x", { name: "Copy", source: "two" }),
      server("x", { name: "Copy", source: "three" }),
    ];
    const sources = (list: Server[]) => list.map((s) => s.source);
    for (const column of ["name", "proxy_ms", "source"] as const) {
      for (const descending of [false, true]) {
        expect(
          sources(sortServers(copies, [], by(column, descending))),
          `${column} ${descending}`,
        ).toEqual(sources(copies));
      }
    }
  });

  it("goes on to the name and the id when two different texts compare as equal", () => {
    // "é" in one piece and "e" with a combining accent are the same text to the collation.
    const one = server("2", { name: "caf\u00e9" });
    const two = server("1", { name: "cafe\u0301" });
    expect(ids(sortServers([one, two], [], by("name")))).toEqual(["1", "2"]);
    expect(ids(sortServers([two, one], [], by("name", true)))).toEqual([
      "1",
      "2",
    ]);
    expect(ids(sortServers([one, two], [], by("protocol")))).toEqual([
      "1",
      "2",
    ]);
  });

  it("orders inside the chosen source only", () => {
    const sorted = visibleServers(
      fixture.servers,
      "main",
      fixture.subscriptions,
      by("proxy_ms"),
    );
    expect(ids(sorted)).toEqual(["a", "b"]);
    expect(
      ids(
        visibleServers(
          fixture.servers,
          "all",
          fixture.subscriptions,
          by("proxy_ms", true),
        ),
      ),
    ).toEqual(["c", "d", "a", "b"]);
  });

  it("goes up, then down, then back to the name on repeated clicks", () => {
    const byName = { column: "name", descending: false };
    expect(DEFAULT_SORT).toEqual(byName);
    expect(nextSort(DEFAULT_SORT, "proxy_ms")).toEqual(by("proxy_ms"));
    expect(nextSort(by("proxy_ms"), "proxy_ms")).toEqual(by("proxy_ms", true));
    expect(nextSort(by("proxy_ms", true), "proxy_ms")).toEqual(byName);
    expect(nextSort(by("proxy_ms", true), "status")).toEqual(by("status"));
    // The name column has no «back to the name»: down, then up again.
    expect(nextSort(by("name"), "name")).toEqual(by("name", true));
    expect(nextSort(by("name", true), "name")).toEqual(byName);
  });
});
