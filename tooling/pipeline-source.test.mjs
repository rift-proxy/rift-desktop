import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { runInNewContext } from "node:vm";
import test from "node:test";
import { githubApi, positiveId, resolveSource, sha } from "./pipeline-source.mjs";

const source = "a".repeat(40);
const baseline = "b".repeat(40);
const repo = "public-owner/rift-desktop";
const pr = { eventName: "pull_request", ref: "refs/pull/17/merge", source, repo, event: { pull_request: { base: { ref: "dev", sha: baseline, repo: { full_name: repo } } } } };
test("PR source is the merge SHA and its baseline is the immutable PR base", async () => {
  assert.deepEqual(await resolveSource(pr, () => assert.fail("PR resolution needs no mutable branch lookup")), { source, baseline, full: false });
  assert.deepEqual(await resolveSource({ ...pr, event: { pull_request: { base: { ...pr.event.pull_request.base, ref: "main" } } } }, () => assert.fail()), { source, baseline, full: false });
});
test("full manual dev fixes both current dev and independent main commits", async () => {
  const resolved = await resolveSource({ eventName: "workflow_dispatch", ref: "refs/heads/dev", source, repo }, async (_method, url) => ({ object: { sha: url.endsWith("/dev") ? source : baseline } }));
  assert.deepEqual(resolved, { source, baseline, full: true });
});
test("wrong events, branches, mutable refs, foreign base and moved dev fail closed", async () => {
  for (const invalid of [
    { ...pr, source: "dev" }, { ...pr, ref: "refs/heads/dev" },
    { ...pr, eventName: "pull_request_target" }, { ...pr, eventName: "push" },
    { ...pr, eventName: "workflow_dispatch", ref: "refs/heads/main" },
    { ...pr, event: { pull_request: { base: { ...pr.event.pull_request.base, ref: "other" } } } },
    { ...pr, event: { pull_request: { base: { ...pr.event.pull_request.base, repo: { full_name: "foreign/repo" } } } } },
    { ...pr, event: { pull_request: { base: { ...pr.event.pull_request.base, sha: "main" } } } },
  ]) await assert.rejects(resolveSource(invalid, () => assert.fail("No branch lookup for invalid context")));
  await assert.rejects(resolveSource({ eventName: "workflow_dispatch", ref: "refs/heads/dev", source, repo }, async () => ({ object: { sha: baseline } })), /moved/);
});
test("SHA and ID validation reject shell syntax and lossy numeric IDs", () => {
  assert.equal(sha(source), source);
  assert.equal(positiveId("123"), 123);
  for (const value of ["dev", "a".repeat(39), "A".repeat(40), "a\n", "$(exit)"]) assert.throws(() => sha(value));
  for (const value of [0, -1, "001", "1.2", "1\n", "9007199254740992", "1;exit"]) assert.throws(() => positiveId(value));
});
test("API rejects foreign origins and missing credentials, and redacts rejected response bodies", async () => {
  assert.throws(() => githubApi(""), /Missing GitHub API token/);
  const api = githubApi("test-only-token", { fetchImpl: () => assert.fail("Foreign URLs must never be requested") });
  for (const endpoint of ["https://foreign.invalid/repos/x/y", "//foreign.invalid/repos/x/y", "http://api.github.com/repos/x/y", "/user", "repos/x/y"])
    await assert.rejects(api("GET", endpoint), /Unexpected/);
  const failing = githubApi("test-only-token", { fetchImpl: async () => ({ ok: false, status: 403, text: async () => "must never be printed" }) });
  await assert.rejects(failing("GET", `/repos/${repo}`), /^Error: GitHub API request failed \(403\)$/);
  const missing = githubApi("test-only-token", { fetchImpl: async () => ({ ok: false, status: 404 }) });
  assert.equal(await missing("GET", `/repos/${repo}`, undefined, { optional: true }), null);
});

test("actual Quality source and final gate scripts reject false greens from skipped selected jobs", () => {
  const yaml = readFileSync(new URL("../.github/workflows/quality.yml", import.meta.url), "utf8");
  const scripts = [...yaml.matchAll(/node --input-type=module <<'NODE'\n([\s\S]*?)\n +NODE/g)].map((match) => match[1].replace(/^          /gm, ""));
  assert.equal(scripts.length, 2, "Keep both executable gate scripts covered");
  const execute = (script, results, full) => runInNewContext(script, { process: { env: { RESULTS: JSON.stringify(results), FULL: full } } });
  for (const full of ["true", "false"]) {
    const selected = ["resolve", "tooling", "rust", "security", "frontend", "perf", full === "true" ? "codeql" : "native-debug"];
    const unselected = full === "true" ? "native-debug" : "codeql";
    const results = Object.fromEntries([...selected.map((name) => [name, { result: "success" }]), [unselected, { result: "skipped" }]]);
    assert.doesNotThrow(() => execute(scripts[0], results, full));
    for (const name of selected) for (const result of ["skipped", "cancelled", "failure", undefined]) {
      const changed = structuredClone(results);
      changed[name] = { result };
      assert.throws(() => execute(scripts[0], changed, full), /did not succeed/);
    }
    const unexpected = structuredClone(results);
    unexpected[unselected].result = "success";
    assert.throws(() => execute(scripts[0], unexpected, full), /Unexpected execution/);
    const final = { resolve: { result: "success" }, "source-gate": { result: "success" }, candidate: { result: full === "true" ? "success" : "skipped" } };
    assert.doesNotThrow(() => execute(scripts[1], final, full));
    for (const name of ["resolve", "source-gate"]) {
      const changed = structuredClone(final);
      changed[name].result = "skipped";
      assert.throws(() => execute(scripts[1], changed, full), /did not succeed/);
    }
    for (const result of ["failure", "cancelled", full === "true" ? "skipped" : "success", undefined]) {
      const changed = structuredClone(final);
      changed.candidate.result = result;
      assert.throws(() => execute(scripts[1], changed, full), /did not finish successfully/);
    }
  }
  for (const script of scripts) for (const full of ["", "yes", undefined]) assert.throws(() => execute(script, {}, full), /resolution/);
});

test("actual CodeQL guard permits only the exact full manual dev SHA", () => {
  const yaml = readFileSync(new URL("../.github/workflows/codeql.yml", import.meta.url), "utf8");
  const script = /node --input-type=module <<'NODE'\n([\s\S]*?)\n +NODE/.exec(yaml)?.[1].replace(/^          /gm, "");
  assert.ok(script);
  const env = { FULL: "true", SOURCE_SHA: source, GITHUB_SHA: source, GITHUB_EVENT_NAME: "workflow_dispatch", GITHUB_REF: "refs/heads/dev" };
  const execute = (change) => runInNewContext(script, { process: { env: { ...env, ...change } } });
  assert.doesNotThrow(() => execute({}));
  for (const change of [{ FULL: "false" }, { GITHUB_EVENT_NAME: "pull_request" }, { GITHUB_EVENT_NAME: "workflow_run" }, { GITHUB_REF: "refs/heads/main" }, { SOURCE_SHA: baseline }, { SOURCE_SHA: "dev" }]) assert.throws(() => execute(change), /full manual dev/);
});

test("signed candidate schedules after expected skips but rejects failed gates and cancellation", () => {
  const yaml = readFileSync(new URL("../.github/workflows/quality.yml", import.meta.url), "utf8");
  const raw = /^  candidate:\n[\s\S]*?^    if:\s*(.+)$/m.exec(yaml)?.[1];
  assert.ok(raw, "Candidate scheduling condition must be present");
  const expression = raw.replace(/^\$\{\{\s*|\s*\}\}$/g, "")
    .replace(/needs\.([\w-]+)/g, (_, name) => `needs[${JSON.stringify(name)}]`);
  // GitHub injects success() unless the predicate contains a status function.
  // Unlike immediate needs, transitive ancestors include intentionally skipped jobs.
  const predicate = /\b(?:success|failure|always|cancelled)\s*\(/.test(expression)
    ? expression : `success() && (${expression})`;
  const scheduled = ({ event = "workflow_dispatch", ref = "refs/heads/dev", full = "true",
    gate = "success", cancelled = false, skippedAncestor = true } = {}) =>
    runInNewContext(predicate, {
      github: { event_name: event, ref },
      needs: { resolve: { result: "success", outputs: { full } }, "source-gate": { result: gate } },
      success: () => !skippedAncestor && gate === "success" && !cancelled,
      failure: () => gate === "failure",
      always: () => true,
      cancelled: () => cancelled,
    });
  assert.equal(scheduled(), true, "Expected upstream skips must not suppress a qualified signed build");
  assert.equal(scheduled({ skippedAncestor: false }), true);
  for (const change of [
    { cancelled: true }, { gate: "failure" }, { gate: "cancelled" }, { gate: "skipped" }, { gate: "" },
    { event: "pull_request" }, { event: "push" }, { ref: "refs/heads/main" },
    { ref: "refs/pull/17/merge" }, { full: "false" }, { full: "" },
  ]) assert.equal(scheduled(change), false, JSON.stringify(change));
});
