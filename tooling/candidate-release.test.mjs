import assert from "node:assert/strict";
import { copyFileSync, mkdirSync, readFileSync, symlinkSync, writeFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { execFileSync, spawnSync } from "node:child_process";
import { artifactName, createCandidate, promoteCandidate, selectCandidate, uploadAsset, validateCandidate } from "./candidate-release.mjs";
import { fixture } from "./release-fixture.mjs";
import { prepareUpdateAssets } from "./prepare-update-assets.mjs";
import { publishNativeSymbols } from "./symbols-archive.mjs";
import { releaseAssets } from "./release-check.mjs";

const source = "a".repeat(40), releaseSha = "b".repeat(40), tree = "c".repeat(40);
const repo = "public-owner/rift-desktop", runId = 12, runAttempt = 2, artifactId = 23;
const selected = { repo, runId, runAttempt, source, tree, releaseSha, artifactId };
const options = { repo, runId: String(runId), releaseSha, eventName: "workflow_dispatch", ref: "refs/heads/main", artifactId };
function candidateFixture(t) {
  const f = fixture(t);
  const directory = path.join(f.base, "candidate");
  mkdirSync(directory);
  const pdb = path.join(f.base, "rift_desktop.pdb"), exe = path.join(f.base, "rift-desktop.exe");
  writeFileSync(exe, "MZ test-only native executable");
  const stage = path.join(directory, "public");
  prepareUpdateAssets("v1.2.3", f.bundle, stage, f.options);
  writeFileSync(pdb, "test-only PDB");
  publishNativeSymbols(pdb, exe, stage, { base: f.base, env: {
    GITHUB_REPOSITORY: repo, SOURCE_SHA: source, SOURCE_TREE: tree, GITHUB_RUN_ID: String(runId), GITHUB_RUN_ATTEMPT: String(runAttempt),
  }, run: (command) => command === "llvm-pdbutil"
    ? "GUID: {01234567-89ab-cdef-0123-456789abcdef}\nAge: 1\n"
    : "PDBGUID: {01234567-89ab-cdef-0123-456789abcdef}\nPDBAge: 1\n" });
  const identity = { source, tree, repo, runId, runAttempt, base: f.base };
  const manifest = createCandidate(directory, identity);
  return { ...f, directory, identity, manifest };
}
function service() {
  const prefix = `/repos/${repo}`;
  const state = {
    metadata: { id: 1, full_name: repo, default_branch: "main" },
    workflow: { id: 7, path: ".github/workflows/quality.yml", state: "active" },
    run: { id: runId, run_attempt: runAttempt, workflow_id: 7, path: ".github/workflows/quality.yml", event: "workflow_dispatch", head_branch: "dev", status: "completed", conclusion: "success", repository: { id: 1, full_name: repo }, head_repository: { id: 1, full_name: repo }, head_sha: source },
    dev: source, main: releaseSha, sourceTree: tree, mainTree: tree,
    artifacts: [{ id: artifactId, name: artifactName, expired: false, workflow_run: { id: runId, head_sha: source } }],
    tag: null, previousRelease: null, remote: null, uploads: new Map(), uploadCalls: [], calls: [],
  };
  const api = async (method, endpoint, body) => {
    state.calls.push({ method, endpoint, body });
    let result;
    if (method === "GET" && endpoint === prefix) result = state.metadata;
    else if (method === "GET" && endpoint === `${prefix}/actions/workflows/quality.yml`) result = state.workflow;
    else if (method === "GET" && endpoint === `${prefix}/actions/runs/${runId}`) result = state.run;
    else if (method === "GET" && endpoint === `${prefix}/git/ref/heads/dev`) result = { object: { sha: state.dev } };
    else if (method === "GET" && endpoint === `${prefix}/git/ref/heads/main`) result = { object: { sha: state.main } };
    else if (method === "GET" && endpoint === `${prefix}/git/commits/${source}`) result = { tree: { sha: state.sourceTree } };
    else if (method === "GET" && endpoint === `${prefix}/git/commits/${releaseSha}`) result = { tree: { sha: state.mainTree } };
    else if (method === "GET" && endpoint === `${prefix}/actions/runs/${runId}/artifacts?per_page=100&page=1`) result = { artifacts: state.artifacts };
    else if (method === "GET" && endpoint === `${prefix}/git/ref/tags/v1.2.3`) result = state.tag;
    else if (method === "GET" && endpoint === `${prefix}/releases?per_page=100&page=1`) result = [state.previousRelease, state.remote].filter(Boolean);
    else if (method === "GET" && endpoint === `${prefix}/releases/tags/v1.2.3`) result = state.previousRelease ?? state.remote;
    else if (method === "POST" && endpoint === `${prefix}/git/refs`) {
      if (state.tag) throw new Error("Tag collision");
      assert.deepEqual(body, { ref: "refs/tags/v1.2.3", sha: releaseSha });
      state.tag = { object: { type: "commit", sha: body.sha } };
      result = state.tag;
    } else if (method === "POST" && endpoint === `${prefix}/releases`) {
      assert.equal(body.draft, true);
      assert.equal(body.target_commitish, releaseSha);
      if (state.remote) throw new Error("Release collision");
      state.remote = { ...body, id: 41, published_at: null, upload_url: `https://uploads.github.com/repos/${repo}/releases/41/assets{?name,label}` };
      result = state.remote;
    } else if (method === "GET" && endpoint === `${prefix}/releases/41`) result = state.remote;
    else if (method === "GET" && ["/assets?per_page=100", "/assets?per_page=100&page=1"].some((suffix) => endpoint === `${prefix}/releases/41${suffix}`)) result = [...state.uploads].map(([name, bytes], index) => ({ id: index + 51, name, size: bytes.length, state: "uploaded" }));
    else if (method === "GET" && /^\/repos\/public-owner\/rift-desktop\/releases\/assets\/[0-9]+$/.test(endpoint)) return [...state.uploads.values()][Number(endpoint.split("/").at(-1)) - 51];
    else assert.fail(`Unexpected request ${method} ${endpoint}`);
    return structuredClone(result);
  };
  const upload = async (url, name, bytes) => {
    assert.equal(url, `https://uploads.github.com/repos/${repo}/releases/41/assets{?name,label}`);
    assert.equal(state.remote.draft, true, "never upload into a published release");
    assert.equal(state.uploads.has(name), false, "never replace an existing asset");
    state.uploadCalls.push(name);
    state.uploads.set(name, Buffer.from(bytes));
  };
  return { state, api, upload };
}
const noWrites = (s) => assert.equal(s.calls.filter((call) => call.method !== "GET").length, 0);

test("candidate contains public assets and exact source/tree/run/attempt hashes only", (t) => {
  const f = candidateFixture(t);
  const result = validateCandidate(f.directory, selected, f.options);
  assert.equal(result.tag, "v1.2.3");
  assert.equal(result.names.length, 5);
  assert.equal(f.manifest.source_sha, source);
  assert.equal(f.manifest.source_tree, tree);
  assert.equal(f.manifest.run_attempt, runAttempt);
  const a = releaseAssets("1.2.3");
  assert.deepEqual(Object.keys(f.manifest.assets).sort(), [a.installer, `${a.installer}.sig`, a.manifest, a.pdb, a.provenance].sort());
  for (const entry of Object.values(f.manifest.assets)) assert.match(entry.sha256, /^[a-f0-9]{64}$/);
  assert.throws(() => createCandidate(f.directory, f.identity), /only public assets/);
});
test("candidate creation refuses a checkout/source or checkout/tree mismatch", (t) => {
  const f = fixture(t);
  for (const actual of [{ actualSource: releaseSha }, { actualTree: source }]) assert.throws(() => createCandidate(f.output, { source, tree, repo, runId, runAttempt, base: f.base, ...actual }), /checkout differs/);
});
test("candidate provenance mismatches, extra fields and altered inventories fail", async (t) => {
  for (const [name, change] of [
    ["source", (m) => { m.source_sha = releaseSha; }], ["tree", (m) => { m.source_tree = source; }],
    ["repository", (m) => { m.repository = "foreign/repo"; }], ["run", (m) => { m.run_id++; }],
    ["attempt", (m) => { m.run_attempt++; }], ["version", (m) => { m.version = "1.2.4"; }],
    ["workflow", (m) => { m.workflow = ".github/workflows/fuzz.yml"; }], ["extra", (m) => { m.private = "not allowed"; }],
    ["hash", (m) => { m.assets["latest.json"].sha256 = "d".repeat(64); }],
    ["size", (m) => { m.assets["latest.json"].size++; }], ["inventory", (m) => { delete m.assets["latest.json"]; }],
  ]) await t.test(name, (t) => {
    const f = candidateFixture(t), m = structuredClone(f.manifest);
    change(m); writeFileSync(path.join(f.directory, "manifest.json"), JSON.stringify(m));
    assert.throws(() => validateCandidate(f.directory, selected, f.options));
  });
});
test("modified bytes, unsafe directories and extra public artifacts fail before promotion", async (t) => {
  await t.test("modified installer", (t) => {
    const f = candidateFixture(t);
    writeFileSync(path.join(f.directory, "public/Rift_1.2.3_x64-setup.exe"), Buffer.concat([f.data, Buffer.from("changed")]));
    assert.throws(() => validateCandidate(f.directory, selected, f.options), /verification/);
  });
  await t.test("PDB deny by exact inventory", (t) => {
    const f = candidateFixture(t);
    writeFileSync(path.join(f.directory, "public/native.pdb"), "private");
    assert.throws(() => validateCandidate(f.directory, selected, f.options), /Unexpected or missing/);
  });
  await t.test("candidate junction/symlink", (t) => {
    const f = candidateFixture(t), linked = path.join(f.base, "linked");
    symlinkSync(f.directory, linked, process.platform === "win32" ? "junction" : "dir");
    assert.throws(() => validateCandidate(linked, selected, f.options), /regular candidate directory/);
  });
});
test("selection verifies the immutable run identity and source/main tree equality", async () => {
  const { api, state } = service();
  assert.deepEqual(await selectCandidate(options, api), selected);
  noWrites(state);
});
test("foreign, PR, failed, stale, wrong workflow and expired/ambiguous candidates are rejected", async (t) => {
  const mutations = {
    "not main default": (s) => { s.metadata.default_branch = "dev"; },
    "wrong workflow": (s) => { s.run.workflow_id = 8; },
    "wrong workflow path": (s) => { s.run.path = ".github/workflows/release.yml"; },
    "inactive workflow": (s) => { s.workflow.state = "disabled_manually"; },
    "PR event": (s) => { s.run.event = "pull_request"; },
    "wrong branch": (s) => { s.run.head_branch = "main"; },
    "failed run": (s) => { s.run.conclusion = "failure"; },
    "incomplete run": (s) => { s.run.status = "in_progress"; },
    "foreign head repository": (s) => { s.run.head_repository.id = 2; },
    "foreign run repository": (s) => { s.run.repository.full_name = "foreign/repo"; },
    "moved dev": (s) => { s.dev = releaseSha; },
    "moved main": (s) => { s.main = source; },
    "different tree": (s) => { s.mainTree = source; },
    "expired artifact": (s) => { s.artifacts[0].expired = true; },
    "wrong artifact run": (s) => { s.artifacts[0].workflow_run.id++; },
    "wrong artifact SHA": (s) => { s.artifacts[0].workflow_run.head_sha = releaseSha; },
    "duplicate artifact": (s) => { s.artifacts.push(structuredClone(s.artifacts[0])); },
    "missing artifact": (s) => { s.artifacts = []; },
  };
  for (const [name, change] of Object.entries(mutations)) await t.test(name, async () => {
    const { state, api } = service(); change(state);
    await assert.rejects(selectCandidate(options, api)); noWrites(state);
  });
  for (const invalid of [{ ...options, ref: "refs/heads/dev" }, { ...options, eventName: "workflow_run" }, { ...options, runId: "12;exit" }]) {
    const { state, api } = service(); await assert.rejects(selectCandidate(invalid, api)); noWrites(state);
  }
});
test("promotion creates a new immutable main tag and draft with the same installer and plain PDB", async (t) => {
  const f = candidateFixture(t), { state, api, upload } = service();
  const result = await promoteCandidate({ ...options, directory: f.directory }, api, upload, f.options);
  assert.deepEqual(result, { tag: "v1.2.3", releaseId: 41, source, release: releaseSha });
  assert.equal(state.tag.object.sha, releaseSha);
  assert.equal(state.remote.draft, true);
  assert.equal(state.uploads.size, 5);
  for (const [name, bytes] of state.uploads) assert.deepEqual(bytes, readFileSync(path.join(f.directory, "public", name)));
  assert.equal(state.uploads.has("manifest.json"), false);
  assert.deepEqual(state.calls.filter((call) => call.method !== "GET").map((call) => call.endpoint), [`/repos/${repo}/git/refs`, `/repos/${repo}/releases`]);
});
test("foreign tags/releases, artifact replacement and rerun attempt mismatch cause no writes", async (t) => {
  for (const name of ["tag", "release", "artifact", "attempt"]) await t.test(name, async (t) => {
    const f = candidateFixture(t), { state, api, upload } = service();
    if (name === "tag") state.tag = { object: { type: "commit", sha: source } };
    if (name === "release") state.previousRelease = { id: 41, tag_name: "v1.2.3" };
    if (name === "artifact") state.artifacts[0].id++;
    if (name === "attempt") state.run.run_attempt++;
    await assert.rejects(promoteCandidate({ ...options, directory: f.directory }, api, upload, f.options)); noWrites(state);
  });
});
test("moved refs or modified remote bytes/inventory fail without tag updates or publishing", async (t) => {
  for (const name of ["moved dev", "moved main", "moved tag", "remote bytes", "extra asset", "published draft"]) await t.test(name, async (t) => {
    const f = candidateFixture(t), { state, api, upload } = service();
    let uploads = 0;
    const changedUpload = async (...args) => {
      await upload(...args); uploads++;
      if (uploads !== Object.keys(f.manifest.assets).length) return;
      if (name === "moved dev") state.dev = releaseSha;
      if (name === "moved main") state.main = source;
      if (name === "moved tag") state.tag.object.sha = source;
      if (name === "remote bytes") state.uploads.set("latest.json", Buffer.alloc(state.uploads.get("latest.json").length));
      if (name === "extra asset") state.uploads.set("manifest.json", Buffer.from("not public"));
      if (name === "published draft") state.remote.draft = false;
    };
    await assert.rejects(promoteCandidate({ ...options, directory: f.directory }, api, changedUpload, f.options));
    assert.equal(state.calls.some((call) => ["PATCH", "DELETE"].includes(call.method)), false);
  });
});

test("promotion reruns recover lost tag/draft responses and partial uploads without replacing anything", async (t) => {
  for (const failure of ["tag response", "draft response", "upload before acceptance", "upload response"]) await t.test(failure, async (t) => {
    const f = candidateFixture(t), { state, api, upload } = service();
    const interruptedApi = async (method, endpoint, ...args) => {
      const result = await api(method, endpoint, ...args);
      if (method === "POST" && ((failure === "tag response" && endpoint.endsWith("/git/refs")) ||
          (failure === "draft response" && endpoint.endsWith("/releases")))) throw new Error("simulated lost response");
      return result;
    };
    let count = 0;
    const interruptedUpload = async (...args) => {
      count++;
      if (failure === "upload before acceptance" && count === 5) throw new Error("simulated upload failure");
      await upload(...args);
      if (failure === "upload response" && count === 5) throw new Error("simulated lost response");
    };
    const promoteOptions = { ...options, directory: f.directory };
    await assert.rejects(promoteCandidate(promoteOptions, interruptedApi, interruptedUpload, f.options), /simulated/);
    const retained = new Map(state.uploads);
    assert.equal(state.tag.object.sha, releaseSha);
    const result = await promoteCandidate(promoteOptions, api, upload, f.options);
    assert.equal(result.releaseId, 41);
    assert.equal(state.uploads.size, 5);
    assert.equal(state.uploadCalls.length, 5, "each asset uploaded only once, including lost responses");
    for (const [name, bytes] of retained) assert.deepEqual(state.uploads.get(name), bytes);
    assert.equal(state.calls.filter((call) => call.method === "POST" && call.endpoint.endsWith("/git/refs")).length, 1);
    assert.equal(state.calls.filter((call) => call.method === "POST" && call.endpoint.endsWith("/releases")).length, 1);
    assert.equal(state.calls.some((call) => ["PATCH", "DELETE"].includes(call.method)), false);
    state.calls.length = 0;
    await promoteCandidate(promoteOptions, api, upload, f.options);
    noWrites(state);
    assert.equal(state.uploadCalls.length, 5, "completed matching drafts are a read-only rerun");
  });
});

test("recovery refuses foreign/published drafts and corrupt assets before any additional upload", async (t) => {
  const mutations = {
    "published": (s) => { s.remote.draft = false; s.remote.published_at = "2026-11-19T00:00:00Z"; },
    "published timestamp": (s) => { s.remote.published_at = "2026-11-19T00:00:00Z"; },
    "foreign target": (s) => { s.remote.target_commitish = source; },
    "foreign candidate": (s) => { s.remote.body = s.remote.body.replace(`artifact ${artifactId}`, "artifact 999"); },
    "foreign manifest": (s) => { s.remote.body = s.remote.body.replace(/manifest SHA-256 [0-9a-f]{64}/, `manifest SHA-256 ${"0".repeat(64)}`); },
    "edited notes": (s) => { s.remote.body += "\nmanual edit"; },
    "foreign endpoint": (s) => { s.remote.upload_url = s.remote.upload_url.replace(repo, "foreign/repo"); },
    "prerelease": (s) => { s.remote.prerelease = true; },
    "missing tag": (s) => { s.tag = null; },
    "moved tag": (s) => { s.tag.object.sha = source; },
    "annotated tag": (s) => { s.tag.object.type = "tag"; },
    "extra asset": (s) => { s.uploads.set("foreign.txt", Buffer.from("foreign")); },
    "same-size corrupt bytes": (s) => { const [name, bytes] = s.uploads.entries().next().value; s.uploads.set(name, Buffer.alloc(bytes.length)); },
  };
  for (const [name, mutate] of Object.entries(mutations)) await t.test(name, async (t) => {
    const f = candidateFixture(t), { state, api, upload } = service();
    let count = 0;
    await assert.rejects(promoteCandidate({ ...options, directory: f.directory }, api, async (...args) => {
      if (++count === 3) throw new Error("simulated interruption");
      await upload(...args);
    }, f.options), /simulated/);
    mutate(state);
    const before = [...state.uploads].map(([name, bytes]) => [name, Buffer.from(bytes)]);
    state.calls.length = 0;
    const uploadsBefore = state.uploadCalls.length;
    await assert.rejects(promoteCandidate({ ...options, directory: f.directory }, api, upload, f.options));
    noWrites(state);
    assert.equal(state.uploadCalls.length, uploadsBefore);
    assert.deepEqual([...state.uploads], before);
  });
});

test("recovery verifies existing upload states and unique inventory before adding missing assets", async (t) => {
  for (const kind of ["starter asset", "duplicate asset"]) await t.test(kind, async (t) => {
    const f = candidateFixture(t), { state, api, upload } = service();
    await assert.rejects(promoteCandidate({ ...options, directory: f.directory }, api, async (...args) => {
      await upload(...args); throw new Error("simulated interruption");
    }, f.options), /simulated/);
    state.calls.length = 0;
    const alteredApi = async (method, endpoint, ...args) => {
      const result = await api(method, endpoint, ...args);
      if (endpoint.includes("/assets?")) {
        if (kind === "starter asset") result[0].state = "starter";
        else result.push({ ...result[0], id: 99 });
      }
      return result;
    };
    await assert.rejects(promoteCandidate({ ...options, directory: f.directory }, alteredApi, upload, f.options), /size\/state|inventory/);
    assert.equal(state.uploadCalls.length, 1);
    noWrites(state);
  });
});

test("recovery finds matching drafts beyond the first release page and rejects ambiguous drafts", async (t) => {
  for (const ambiguous of [false, true]) await t.test(String(ambiguous), async (t) => {
    const f = candidateFixture(t), { state, api, upload } = service();
    await assert.rejects(promoteCandidate({ ...options, directory: f.directory }, api, async () => { throw new Error("simulated interruption"); }, f.options), /simulated/);
    state.calls.length = 0;
    let pageTwo = false;
    const pagedApi = async (method, endpoint, ...args) => {
      if (endpoint.endsWith("/releases?per_page=100&page=1"))
        return Array.from({ length: 100 }, (_, index) => ({ tag_name: `v0.0.${index}` }));
      if (endpoint.endsWith("/releases?per_page=100&page=2")) {
        pageTwo = true;
        return ambiguous ? [state.remote, { ...state.remote, id: 42 }] : [state.remote];
      }
      return api(method, endpoint, ...args);
    };
    if (ambiguous) await assert.rejects(promoteCandidate({ ...options, directory: f.directory }, pagedApi, upload, f.options), /Ambiguous/);
    else await promoteCandidate({ ...options, directory: f.directory }, pagedApi, upload, f.options);
    assert.equal(pageTwo, true);
    noWrites(state);
    assert.equal(state.uploads.size, ambiguous ? 0 : 5);
  });
});

test("recovery rechecks refs and publication after verifying retained asset bytes", async (t) => {
  for (const kind of ["dev", "main", "tag", "published"]) await t.test(kind, async (t) => {
    const f = candidateFixture(t), { state, api, upload } = service();
    await assert.rejects(promoteCandidate({ ...options, directory: f.directory }, api, async (...args) => {
      await upload(...args); throw new Error("simulated interruption");
    }, f.options), /simulated/);
    state.calls.length = 0;
    const movingApi = async (method, endpoint, ...args) => {
      const result = await api(method, endpoint, ...args);
      if (endpoint.includes("/releases/assets/")) {
        if (kind === "dev") state.dev = releaseSha;
        if (kind === "main") state.main = source;
        if (kind === "tag") state.tag = null;
        if (kind === "published") { state.remote.draft = false; state.remote.published_at = "2026-11-19T00:00:00Z"; }
      }
      return result;
    };
    await assert.rejects(promoteCandidate({ ...options, directory: f.directory }, movingApi, upload, f.options), /references moved|tag disappeared|unpublished/);
    noWrites(state);
    assert.equal(state.uploadCalls.length, 1);
  });
});
test("explicit owner removal of a damaged attributable draft permits recovery without touching its tag", async (t) => {
  const f = candidateFixture(t), { state, api, upload } = service();
  const promoteOptions = { ...options, directory: f.directory };
  await assert.rejects(promoteCandidate(promoteOptions, api, async (...args) => {
    await upload(...args); throw new Error("simulated interruption");
  }, f.options), /simulated/);
  const tag = state.tag;
  const name = state.uploads.keys().next().value;
  state.uploads.set(name, Buffer.from("damaged upload"));
  await assert.rejects(promoteCandidate(promoteOptions, api, upload, f.options), /size\/state/);
  assert.match(state.remote.body, new RegExp(`Quality run ${runId} attempt ${runAttempt}; artifact ${artifactId}`));
  assert.equal(state.remote.draft, true);
  // The owner deletes only this verified unpublished draft in GitHub UI.
  state.remote = null;
  state.uploads.clear();
  const result = await promoteCandidate(promoteOptions, api, upload, f.options);
  assert.equal(result.tag, "v1.2.3");
  assert.equal(state.tag, tag);
  assert.equal(state.uploads.size, 5);
  assert.equal(state.calls.filter((call) => call.method === "POST" && call.endpoint.endsWith("/git/refs")).length, 1);
  assert.equal(state.calls.filter((call) => call.method === "POST" && call.endpoint.endsWith("/releases")).length, 2);
  assert.equal(state.calls.some((call) => ["PATCH", "DELETE"].includes(call.method)), false);
});

test("upload endpoint cannot redirect a token or accept credentials/foreign origins", async () => {
  for (const url of ["https://foreign.invalid/repos/a/b/releases/1/assets", "https://uploads.github.com@foreign.invalid/repos/a/b/releases/1/assets", "https://token@uploads.github.com/repos/a/b/releases/1/assets", "http://uploads.github.com/repos/a/b/releases/1/assets", "https://uploads.github.com/repos/a/b/releases/1/assets?secret=x"])
    await assert.rejects(uploadAsset("test-only-token", url, "latest.json", Buffer.from("x"), () => assert.fail("Unsafe URL must not be requested")), /Unexpected/);
  let observed;
  await uploadAsset("test-only-token", `https://uploads.github.com/repos/${repo}/releases/41/assets{?name,label}`, "latest.json", Buffer.from("x"), async (url, init) => { observed = { url, init }; return { ok: true, json: async () => ({ id: 1 }) }; });
  assert.equal(observed.url.searchParams.get("name"), "latest.json");
  assert.equal(observed.init.redirect, "error");
});

test("a racing tag collision is never force-updated or converted into reuse", async (t) => {
  const f = candidateFixture(t), { state, api, upload } = service();
  const racingApi = async (method, endpoint, ...args) => {
    if (method === "POST" && endpoint.endsWith("/git/refs")) state.tag = { object: { type: "commit", sha: source } };
    return api(method, endpoint, ...args);
  };
  await assert.rejects(promoteCandidate({ ...options, directory: f.directory }, racingApi, upload, f.options), /Tag collision/);
  assert.equal(state.tag.object.sha, source);
  assert.equal(state.uploads.size, 0);
  assert.equal(state.calls.some((call) => ["PATCH", "DELETE"].includes(call.method) || (call.method === "POST" && call.endpoint.endsWith("/releases"))), false);
});

test("candidate selection paginates within the authenticated run instead of global artifact name lookup", async () => {
  const { state, api } = service();
  let secondPage = false;
  const pagedApi = async (method, endpoint, ...args) => {
    if (endpoint.endsWith("/artifacts?per_page=100&page=1")) return { artifacts: Array.from({ length: 100 }, (_, index) => ({ id: 1000 + index, name: `other-${index}` })) };
    if (endpoint.endsWith("/artifacts?per_page=100&page=2")) { secondPage = true; return { artifacts: state.artifacts }; }
    return api(method, endpoint, ...args);
  };
  assert.deepEqual(await selectCandidate(options, pagedApi), selected);
  assert.equal(secondPage, true);
  noWrites(state);
});

test("foreign feed, signature swap, PDB tampering and foreign provenance fail before writes", async (t) => {
  for (const kind of ["feed", "signature", "pdb", "source", "debug identity", "extra pdb"]) await t.test(kind, async (t) => {
    const f = candidateFixture(t), s = service();
    const dir = path.join(f.directory, "public"), assets = releaseAssets("1.2.3");
    if (kind === "feed") {
      const file = path.join(dir, assets.manifest), record = JSON.parse(readFileSync(file));
      record.platforms["windows-x86_64"].url = "https://github.com/foreign/repo/releases/download/v1.2.3/setup.exe";
      writeFileSync(file, JSON.stringify(record));
    }
    if (kind === "signature") writeFileSync(path.join(dir, assets.installer + ".sig"), fixture(t).signature());
    if (kind === "pdb") writeFileSync(path.join(dir, assets.pdb), "tampered PDB");
    if (kind === "extra pdb") writeFileSync(path.join(dir, "third-party.pdb"), "unrelated");
    if (["source", "debug identity"].includes(kind)) {
      const file = path.join(dir, assets.provenance), record = JSON.parse(readFileSync(file));
      if (kind === "source") record.source_sha = releaseSha;
      else record.debug_id.guid = "not-a-guid";
      writeFileSync(file, JSON.stringify(record));
    }
    await assert.rejects(promoteCandidate({ ...options, directory: f.directory }, s.api, s.upload, f.options));
    noWrites(s.state);
  });
});

test("CLI smoke prepares a signed installer and creates the real candidate from a disposable Git checkout", (t) => {
  const f = fixture(t);
  const tooling = path.join(f.base, "tooling");
  mkdirSync(tooling);
  for (const name of ["prepare-update-assets.mjs", "release-check.mjs", "candidate-release.mjs", "pipeline-source.mjs"])
    copyFileSync(path.join(import.meta.dirname, name), path.join(tooling, name));
  const git = (...args) => execFileSync("git", args, { cwd: f.base, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
  git("init", "--quiet");
  git("add", ".");
  git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false", "commit", "--quiet", "-m", "disposable signed asset fixture");
  const actualSource = git("rev-parse", "HEAD"), actualTree = git("rev-parse", "HEAD^{tree}");
  const env = { PATH: process.env.PATH, ...(process.env.SystemRoot ? { SystemRoot: process.env.SystemRoot } : {}),
    GITHUB_REPOSITORY: repo, GITHUB_RUN_ID: String(runId), GITHUB_RUN_ATTEMPT: String(runAttempt),
    GITHUB_EVENT_NAME: "workflow_dispatch", GITHUB_REF: "refs/heads/dev", SOURCE_SHA: actualSource, SOURCE_TREE: actualTree };
  const cli = (name, args, extra = {}) => {
    const result = spawnSync(process.execPath, [path.join(tooling, name), ...args], { cwd: f.base, env: { ...env, ...extra }, encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
    return result;
  };
  const directory = path.join(f.base, "candidate"), publicDir = path.join(directory, "public");
  mkdirSync(directory);
  const pdb = path.join(f.base, "rift_desktop.pdb"), exe = path.join(f.base, "rift-desktop.exe");
  writeFileSync(exe, "MZ fixture executable; LLVM identity is test-injected, not a real PE/PDB");
  cli("prepare-update-assets.mjs", ["v1.2.3", f.bundle, publicDir]);
  writeFileSync(pdb, "fixture PDB; not a real PDB");
  publishNativeSymbols(pdb, exe, publicDir, { base: f.base, env,
    run: (command) => command === "llvm-pdbutil"
      ? "GUID: {01234567-89ab-cdef-0123-456789abcdef}\nAge: 1\n"
      : "PDBGUID: {01234567-89ab-cdef-0123-456789abcdef}\nPDBAge: 1\n" });
  cli("candidate-release.mjs", ["create", directory]);
  cli("release-check.mjs", ["assets", "v1.2.3", publicDir]);
  const result = validateCandidate(directory, { ...selected, source: actualSource, tree: actualTree }, f.options);
  assert.equal(result.names.length, 5);
  assert.equal(result.manifest.source_tree, actualTree);
});
