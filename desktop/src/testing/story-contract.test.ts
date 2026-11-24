import { describe, expect, it } from "vitest";
import type { Server, Snapshot } from "../backend/types";
import reference from "./core-contract.json";
import { fixture, MockBackend, type ProfileContents } from "./mock";

// Match the Rust example's two-server/one-source profile, with deliberately different
// user preferences from the starting snapshot so a partial import cannot pass.
const picked: ProfileContents = {
  settings: {
    mode: "tun_proxy",
    theme: "dark",
    language: "en",
    auto_select: false,
    proxy_port: 3090,
    api_port: 3091,
    interval_secs: 90,
    pool_size: 5,
    full_interval_mins: 20,
    degrade_percent: 40,
    degrade_cap_ms: 1800,
    endpoint_timeout_ms: 2000,
    proxy_timeout_ms: 6000,
    concurrency: 2,
    connectivity_url: "https://connectivity.example/check",
    subscription_interval_mins: 120,
    switch_threshold_percent: 40,
    switch_threshold_ms: 80,
    autostart: true,
    auto_connect: true,
    geodata_auto_update: false,
    find_process: true,
  },
  servers: [
    { ...fixture.servers[3], id: "picked-manual" },
    { ...fixture.servers[0], id: "picked-source", source: "picked-sub" },
  ],
  subscriptions: [{ ...fixture.subscriptions[0], id: "picked-sub", count: 1 }],
  selected: "picked-manual",
  routing: {
    order: ["direct", "block", "proxy"],
    direct: ["domain:intranet.example"],
    block: ["domain:blocked.example"],
    proxy: ["domain:remote.example"],
  },
  servers_sort: { column: "checked_at", descending: true },
};

function expectUnchecked(server: Server) {
  expect(server).toEqual({
    ...reference.unchecked_server,
    id: server.id,
    name: server.name,
    protocol: server.protocol,
    source: null,
  });
}

describe("Storybook double against the Rust contract reference", () => {
  it("reports the current app version and keeps every settings default except the story theme", async () => {
    const backend = new MockBackend();
    expect(await backend.appVersion()).toBe(reference.app_version);
    expect(fixture.settings).toEqual({
      ...reference.store_defaults.settings,
      theme: "light",
    });
    expect(fixture.routing).toEqual(reference.store_defaults.routing);
    expect(fixture.servers_sort).toEqual(reference.store_defaults.servers_sort);
    expect(Object.keys(picked.settings).sort()).toEqual(
      Object.keys(reference.store_defaults.settings).sort(),
    );
  });

  it("describes the picked profile with Rust's current version and summary fields", async () => {
    const backend = new MockBackend();
    backend.profileFile = structuredClone(picked);
    const preview = await backend.inspectProfile();
    expect(preview).not.toBeNull();
    const { request_id, ...summary } = preview!;
    expect(request_id).toBeGreaterThan(0);
    expect(summary).toEqual(reference.profile_summary);
    expect(summary.version).toBe(reference.profile_version);
  });

  it.each([false, true])(
    "imports all user data and emits it, preserving local autostart=%s",
    async (autostart) => {
      const initial = structuredClone(fixture);
      initial.settings.autostart = autostart;
      const backend = new MockBackend(initial);
      const contents = structuredClone(picked);
      contents.settings.autostart = !autostart;
      backend.profileFile = contents;
      const seen: Snapshot[] = [];
      await backend.subscribe((state) => seen.push(state));
      const preview = await backend.inspectProfile();
      if (!preview) throw new Error("No picked contract profile");
      const applied = await backend.importProfile(preview.request_id);
      expect(applied.settings).toEqual({ ...contents.settings, autostart });
      expect(applied.servers).toEqual(contents.servers);
      expect(applied.subscriptions).toEqual(contents.subscriptions);
      expect(applied.selected).toBe(contents.selected);
      expect(applied.routing).toEqual(contents.routing);
      expect(applied.servers_sort).toEqual(contents.servers_sort);
      expect(seen.at(-1)).toEqual(applied);
      expect(await backend.snapshot()).toEqual(applied);
      // The inspected file was copied; editing the next picker result cannot mutate state.
      contents.routing.direct.push("domain:later.example");
      expect((await backend.snapshot()).routing.direct).toEqual(
        picked.routing.direct,
      );
    },
  );

  it("never inherits measurements or check timestamps when previewing or adding servers", async () => {
    const backend = new MockBackend();
    expectUnchecked(await backend.preview(reference.server_uri));
    const preview = await backend.previewInput(reference.server_uri);
    if (preview.kind !== "servers") throw new Error("No server preview");
    expect(preview.items).toHaveLength(1);
    expectUnchecked(preview.items[0].server);
    const before = (await backend.snapshot()).servers.length;
    await backend.execute({ type: "add_server", uri: reference.server_uri });
    await backend.execute({
      type: "add_servers",
      uris: [
        reference.server_uri.replace("@manual.example:", "@second.example:"),
        reference.server_uri.replace("@manual.example:", "@third.example:"),
      ],
    });
    const added = (await backend.snapshot()).servers.slice(before);
    expect(added).toHaveLength(3);
    expect(new Set(added.map((server) => server.id)).size).toBe(3);
    for (const server of added) {
      expectUnchecked(server);
      expect(await backend.history(server.id)).toEqual([]);
    }
  });

  it("keeps fixture subscription counts, source references and selection internally consistent", async () => {
    const state = await new MockBackend().snapshot();
    expect(new Set(state.servers.map((server) => server.id)).size).toBe(
      state.servers.length,
    );
    expect(new Set(state.subscriptions.map((source) => source.id)).size).toBe(
      state.subscriptions.length,
    );
    for (const source of state.subscriptions) {
      expect(source.count).toBe(
        state.servers.filter((server) => server.source === source.id).length,
      );
    }
    for (const server of state.servers) {
      if (server.source !== null) {
        expect(
          state.subscriptions.some((source) => source.id === server.source),
        ).toBe(true);
      }
    }
    expect(state.selected).not.toBeNull();
    expect(state.servers.some((server) => server.id === state.selected)).toBe(
      true,
    );
  });
});
