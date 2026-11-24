import { describe, expect, it } from "vitest";
import type { Snapshot } from "../backend/types";
import { MockBackend, fixture, type ProfileContents } from "./mock";

// The mock stands in for the Rust host in every UI test, so it has to keep the observable
// rules of `import_profile`: the file replaces the data, the checked request is single use,
// and a failed apply leaves the data alone and the request retryable.
const picked: ProfileContents = {
  settings: { ...fixture.settings, theme: "dark", proxy_port: 3090 },
  servers: fixture.servers.slice(0, 2),
  subscriptions: fixture.subscriptions.slice(0, 1),
  selected: null,
  routing: {
    order: ["proxy", "direct", "block"],
    block: ["example.com"],
    direct: [],
    proxy: [],
  },
  servers_sort: { column: "status", descending: true },
};

async function stage(backend: MockBackend) {
  const preview = await backend.inspectProfile();
  if (!preview) throw new Error("the mock picked no file");
  return preview;
}

describe("MockBackend profile import", () => {
  it("describes the picked file, not the current state", async () => {
    const backend = new MockBackend();
    backend.profileFile = picked;
    expect(await stage(backend)).toMatchObject({
      version: 2,
      servers: 2,
      manual_servers: 0,
      subscriptions: 1,
      has_selected_server: false,
    });
  });

  it("replaces settings, servers, subscriptions and selection, and tells subscribers", async () => {
    const backend = new MockBackend();
    backend.profileFile = picked;
    const seen: Snapshot[] = [];
    await backend.subscribe((state) => seen.push(state));
    const { request_id } = await stage(backend);

    const applied = await backend.importProfile(request_id);

    expect(applied.settings).toEqual(picked.settings);
    expect(applied.servers).toEqual(picked.servers);
    expect(applied.subscriptions).toEqual(picked.subscriptions);
    expect(applied.selected).toBeNull();
    expect(applied.routing).toEqual(picked.routing);
    expect(applied.servers_sort).toEqual(picked.servers_sort);
    expect(seen.at(-1)).toEqual(applied);
    expect(await backend.snapshot()).toEqual(applied);
  });

  it("spends the request id: it cannot be applied twice or after a newer pick", async () => {
    const backend = new MockBackend();
    const first = await stage(backend);
    await backend.importProfile(first.request_id);
    await expect(backend.importProfile(first.request_id)).rejects.toMatch(
      "больше недоступен",
    );

    const second = await stage(backend);
    const third = await stage(backend);
    expect(third.request_id).not.toBe(second.request_id);
    await expect(backend.importProfile(second.request_id)).rejects.toMatch(
      "больше недоступен",
    );
    await expect(backend.importProfile(third.request_id)).resolves.toBeTruthy();
  });

  it("drops the checked file when the Open dialog is cancelled", async () => {
    const backend = new MockBackend();
    const { request_id } = await stage(backend);
    backend.profileFile = null;
    expect(await backend.inspectProfile()).toBeNull();
    await expect(backend.importProfile(request_id)).rejects.toMatch(
      "больше недоступен",
    );
  });

  it("keeps the data and the request when applying fails, and records the core error", async () => {
    const backend = new MockBackend();
    backend.profileFile = picked;
    const { request_id } = await stage(backend);
    backend.failProfile(
      "import",
      "Не удалось применить профиль; текущие данные сохранены",
      "Ошибка чтения или сохранения данных",
    );

    await expect(backend.importProfile(request_id)).rejects.toBe(
      "Не удалось применить профиль; текущие данные сохранены",
    );
    const after = await backend.snapshot();
    expect(after.settings).toEqual(fixture.settings);
    expect(after.servers).toEqual(fixture.servers);
    expect(after.error).toBe("Ошибка чтения или сохранения данных");

    // The same request is still valid, as `ProfileImports::restore` guarantees.
    const retried = await backend.importProfile(request_id);
    expect(retried.settings).toEqual(picked.settings);
  });

  it("keeps a failure without a core message out of Snapshot.error", async () => {
    const backend = new MockBackend();
    backend.failProfile("export", "Не удалось сохранить профиль");
    await expect(backend.exportProfile()).rejects.toBe(
      "Не удалось сохранить профиль",
    );
    expect((await backend.snapshot()).error).toBeNull();
    expect(backend.profileExported).toBe(false);
    await expect(backend.exportProfile()).resolves.toBe(true);
    expect(backend.profileExported).toBe(true);
  });

  it("holds a step until it is released", async () => {
    const backend = new MockBackend();
    const release = backend.holdProfile("export");
    let answered = false;
    const pending = backend.exportProfile().then((saved) => {
      answered = true;
      return saved;
    });
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(answered).toBe(false);
    release();
    await expect(pending).resolves.toBe(true);
  });

  it.each([false, true])("preserves local autostart=%s", async (autostart) => {
    const backend = new MockBackend({
      ...fixture,
      settings: { ...fixture.settings, autostart },
    });
    backend.profileFile = {
      ...picked,
      settings: { ...picked.settings, autostart: !autostart },
    };
    const { request_id } = await stage(backend);
    expect((await backend.importProfile(request_id)).settings.autostart).toBe(
      autostart,
    );
  });
});

// The core clears the `Snapshot.error` that a profile step recorded when a later profile step
// succeeds, and leaves an error from anywhere else alone. The mock has to say the same, or the
// banner in UI tests and stories would contradict the success message that follows a failure.
type Step = Parameters<MockBackend["failProfile"]>[0];
const coreText = "Ошибка чтения или сохранения данных";
const crashText = "Процесс sing-box неожиданно завершился";

// The import applies the file that an earlier pick checked, as in the dialog flow.
async function stepRunner(backend: MockBackend, step: Step) {
  const { request_id } =
    step === "import" ? await stage(backend) : { request_id: 0 };
  return () =>
    step === "export"
      ? backend.exportProfile()
      : step === "inspect"
        ? backend.inspectProfile()
        : backend.importProfile(request_id);
}

describe("MockBackend profile errors in Snapshot.error", () => {
  it.each<Step>(["export", "inspect", "import"])(
    "clears the error a failed %s recorded when the retry succeeds, and tells subscribers",
    async (step) => {
      const backend = new MockBackend();
      const run = await stepRunner(backend, step);
      backend.failProfile(step, "Не удалось выполнить шаг профиля", coreText);
      const seen: Snapshot[] = [];
      await backend.subscribe((state) => seen.push(state));

      await expect(run()).rejects.toBe("Не удалось выполнить шаг профиля");
      expect((await backend.snapshot()).error).toBe(coreText);
      expect(seen.at(-1)?.error).toBe(coreText);

      await run();
      expect((await backend.snapshot()).error).toBeNull();
      expect(seen.at(-1)?.error).toBeNull();
    },
  );

  it("keeps an error that was there before and no profile step wrote", async () => {
    const backend = new MockBackend({ ...fixture, error: crashText });
    await backend.exportProfile();
    const { request_id } = await stage(backend);
    await backend.importProfile(request_id);
    expect((await backend.snapshot()).error).toBe(crashText);
  });

  it("keeps a foreign error that replaced the profile one, even with the same text", async () => {
    const backend = new MockBackend();
    backend.failProfile("export", "Не удалось сохранить профиль", coreText);
    await expect(backend.exportProfile()).rejects.toBeTruthy();
    backend.recordCoreError(crashText);
    await backend.exportProfile();
    expect((await backend.snapshot()).error).toBe(crashText);

    backend.failProfile("export", "Не удалось сохранить профиль", coreText);
    await expect(backend.exportProfile()).rejects.toBeTruthy();
    backend.recordCoreError(coreText);
    await backend.exportProfile();
    expect((await backend.snapshot()).error).toBe(coreText);
  });

  it("does not treat a cancelled Open dialog as a success", async () => {
    const backend = new MockBackend();
    backend.failProfile("inspect", "Некорректный формат профиля", coreText);
    await expect(backend.inspectProfile()).rejects.toBeTruthy();
    backend.profileFile = null;
    expect(await backend.inspectProfile()).toBeNull();
    expect((await backend.snapshot()).error).toBe(coreText);

    backend.profileFile = picked;
    await stage(backend);
    expect((await backend.snapshot()).error).toBeNull();
  });
});

describe("MockBackend manual imports", () => {
  it("keeps identifiers unique across batches and removes just the chosen server", async () => {
    const backend = new MockBackend();
    const before = (await backend.snapshot()).servers.length;
    for (const uri of ["vless://one", "trojan://two"]) {
      await backend.execute({ type: "add_servers", uris: [uri] });
    }
    const added = (await backend.snapshot()).servers.slice(before);
    expect(new Set(added.map((server) => server.id)).size).toBe(2);
    await backend.execute({ type: "remove_server", id: added[0].id });
    expect(
      (await backend.snapshot()).servers.some(
        (server) => server.id === added[1].id,
      ),
    ).toBe(true);
  });

  it("accepts all four schemes and leaves the entire batch unapplied on failure", async () => {
    const backend = new MockBackend();
    await backend.execute({
      type: "add_servers",
      uris: ["vless://one", "vmess://two", "trojan://three", "ss://four"],
    });
    expect(
      (await backend.snapshot()).servers
        .slice(-4)
        .map((server) => server.protocol),
    ).toEqual(["vless", "vmess", "trojan", "shadowsocks"]);
    const before = await backend.snapshot();
    await expect(
      backend.execute({ type: "add_servers", uris: ["vless://ok", "bad"] }),
    ).rejects.toBeTruthy();
    expect(await backend.snapshot()).toEqual(before);
  });

  it("does not inherit measurements in previews or imported servers", async () => {
    const backend = new MockBackend();
    const fresh = {
      status: "unchecked",
      checked_at: null,
      retry_at: null,
      endpoint_ms: null,
      proxy_ms: null,
      failure_code: null,
      excluded: false,
    };
    expect(await backend.preview("trojan://one")).toMatchObject(fresh);
    const preview = await backend.previewInput("trojan://one ss://two");
    expect(preview.kind).toBe("servers");
    if (preview.kind !== "servers") throw new Error("expected servers");
    expect(preview.items.map(({ server }) => server.protocol)).toEqual([
      "trojan",
      "shadowsocks",
    ]);
    for (const { server } of preview.items) expect(server).toMatchObject(fresh);
    await backend.execute({ type: "add_server", uri: "vless://one" });
    await backend.execute({ type: "add_servers", uris: ["trojan://two"] });
    for (const server of (await backend.snapshot()).servers.slice(-2))
      expect(server).toMatchObject(fresh);
  });
});
