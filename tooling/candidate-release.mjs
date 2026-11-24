#!/usr/bin/env node
// Promote the qualified installer and its matching audited native PDB without rebuilding.
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { appendFileSync, lstatSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { checkVersions, repository, tagVersion, validateReleaseAssets, releaseAssets } from "./release-check.mjs";
import { githubApi, positiveId, sha } from "./pipeline-source.mjs";

const root = path.resolve(import.meta.dirname, "..");
const workflow = ".github/workflows/quality.yml";
export const artifactName = "signed-windows-candidate";
const digest = (data) => createHash("sha256").update(data).digest("hex");
const keys = (value, expected) => value && typeof value === "object" && !Array.isArray(value) && JSON.stringify(Object.keys(value).sort()) === JSON.stringify([...expected].sort());
const json = (file) => JSON.parse(readFileSync(file, "utf8"));
function safeDirectory(directory) {
  if (!lstatSync(directory).isDirectory() || lstatSync(directory).isSymbolicLink()) throw new Error("Expected a regular candidate directory");
}
function assetHashes(directory, names) {
  return Object.fromEntries(names.map((name) => {
    const bytes = readFileSync(path.join(directory, name));
    return [name, { sha256: digest(bytes), size: bytes.length }];
  }));
}
function validateNativeProvenance(directory, version, identity) {
  const assets = releaseAssets(version);
  const record = json(path.join(directory, assets.provenance));
  if (!keys(record, ["schema", "repository", "source_sha", "source_tree", "version", "run_id", "run_attempt", "kind", "debug_id", "pdb", "files", "executable"]) ||
      record.schema !== 1 || record.kind !== "native" || record.pdb !== assets.pdb ||
      record.repository !== identity.repo || record.source_sha !== identity.source || record.source_tree !== identity.tree || record.version !== version ||
      record.run_id !== identity.runId || record.run_attempt !== identity.runAttempt ||
      !keys(record.debug_id, ["guid", "age"]) || !/^[0-9a-f]{32}$/.test(record.debug_id.guid) ||
      !Number.isInteger(record.debug_id.age) || record.debug_id.age < 0 || record.debug_id.age > 0xffffffff ||
      !keys(record.files, [assets.pdb, assets.installer]))
    throw new Error("Native symbol provenance does not match the qualified source/run");
  for (const [name, actual] of Object.entries(assetHashes(directory, [assets.pdb, assets.installer]))) {
    if (!keys(record.files[name], ["sha256", "size"]) || record.files[name].sha256 !== actual.sha256 || record.files[name].size !== actual.size)
      throw new Error("Native symbol inventory hash/size mismatch");
  }
  if (!keys(record.executable, ["sha256", "size"]) || !/^[0-9a-f]{64}$/.test(record.executable.sha256) || !Number.isSafeInteger(record.executable.size) || record.executable.size < 1)
    throw new Error("Invalid native executable identity");
}
export function createCandidate(directory, { source, tree, repo, runId, runAttempt, actualSource = source, actualTree = tree, base = root }) {
  repository(repo);
  sha(source); sha(tree);
  if (sha(actualSource) !== source || sha(actualTree) !== tree) throw new Error("Candidate checkout differs from resolved source");
  positiveId(runId); positiveId(runAttempt);
  safeDirectory(directory);
  if (JSON.stringify(readdirSync(directory).sort()) !== JSON.stringify(["public"])) throw new Error("Candidate output must start with only public assets");
  const publicDir = path.join(directory, "public");
  safeDirectory(publicDir);
  const version = json(path.join(base, "desktop/src-tauri/tauri.conf.json")).version;
  const names = validateReleaseAssets(`v${version}`, publicDir, { base, repo });
  validateNativeProvenance(publicDir, version, { repo, source, tree, runId: positiveId(runId), runAttempt: positiveId(runAttempt) });
  const manifest = { schema: 2, workflow, repository: repo, source_sha: source, source_tree: tree, run_id: positiveId(runId), run_attempt: positiveId(runAttempt), version, assets: assetHashes(publicDir, names) };
  writeFileSync(path.join(directory, "manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`, { flag: "wx" });
  return manifest;
}
export function validateCandidate(directory, selected, { base = root } = {}) {
  safeDirectory(directory);
  if (JSON.stringify(readdirSync(directory).sort()) !== JSON.stringify(["manifest.json", "public"])) throw new Error("Unexpected candidate contents");
  if (!lstatSync(path.join(directory, "manifest.json")).isFile() || lstatSync(path.join(directory, "manifest.json")).isSymbolicLink()) throw new Error("Candidate manifest must be a regular file");
  safeDirectory(path.join(directory, "public"));
  const manifest = json(path.join(directory, "manifest.json"));
  if (!keys(manifest, ["schema", "workflow", "repository", "source_sha", "source_tree", "run_id", "run_attempt", "version", "assets"]) || manifest.schema !== 2 || manifest.workflow !== workflow || manifest.repository !== selected.repo || manifest.source_sha !== selected.source || manifest.source_tree !== selected.tree || manifest.run_id !== selected.runId || manifest.run_attempt !== selected.runAttempt)
    throw new Error("Candidate provenance does not match the verified Quality run");
  const tag = `v${manifest.version}`;
  tagVersion(tag);
  checkVersions(tag, base);
  const names = validateReleaseAssets(tag, path.join(directory, "public"), { base, repo: selected.repo });
  validateNativeProvenance(path.join(directory, "public"), manifest.version, selected);
  if (!keys(manifest.assets, names)) throw new Error("Candidate asset inventory differs from the public allowlist");
  const actual = assetHashes(path.join(directory, "public"), names);
  for (const name of names) if (!keys(manifest.assets[name], ["sha256", "size"]) || manifest.assets[name].sha256 !== actual[name].sha256 || manifest.assets[name].size !== actual[name].size)
    throw new Error("Candidate asset hash/size mismatch");
  return { manifest, names, tag, directory: path.join(directory, "public") };
}
export async function selectCandidate({ repo, runId, releaseSha, eventName, ref }, api) {
  repository(repo); positiveId(runId); sha(releaseSha);
  if (eventName !== "workflow_dispatch" || ref !== "refs/heads/main") throw new Error("Release promotion must be dispatched on main");
  const prefix = `/repos/${repo}`;
  const metadata = await api("GET", prefix);
  if (metadata.full_name !== repo || metadata.default_branch !== "main") throw new Error("Release requires this repository's main default branch");
  const expectedWorkflow = await api("GET", `${prefix}/actions/workflows/quality.yml`);
  if (expectedWorkflow.path !== workflow || expectedWorkflow.state !== "active") throw new Error("Expected the active Quality workflow");
  const run = await api("GET", `${prefix}/actions/runs/${positiveId(runId)}`);
  if (run.id !== positiveId(runId) || run.workflow_id !== expectedWorkflow.id || run.path !== workflow || run.event !== "workflow_dispatch" || run.head_branch !== "dev" || run.status !== "completed" || run.conclusion !== "success" || run.repository?.id !== metadata.id || run.repository?.full_name !== repo || run.head_repository?.id !== metadata.id || run.head_repository?.full_name !== repo)
    throw new Error("Candidate must come from a successful same-repository manual dev Quality run");
  const source = sha(run.head_sha);
  const runAttempt = positiveId(run.run_attempt);
  const dev = await api("GET", `${prefix}/git/ref/heads/dev`);
  const main = await api("GET", `${prefix}/git/ref/heads/main`);
  if (sha(dev.object?.sha) !== source || sha(main.object?.sha) !== releaseSha) throw new Error("dev/main moved; reject the stale candidate or release dispatch");
  const sourceCommit = await api("GET", `${prefix}/git/commits/${source}`);
  const mainCommit = await api("GET", `${prefix}/git/commits/${releaseSha}`);
  const tree = sha(sourceCommit.tree?.sha);
  if (tree !== sha(mainCommit.tree?.sha)) throw new Error("Candidate and main must have identical Git trees after squash");
  const artifacts = [];
  for (let page = 1; ; page++) {
    const batch = await api("GET", `${prefix}/actions/runs/${positiveId(runId)}/artifacts?per_page=100&page=${page}`);
    if (!Array.isArray(batch.artifacts)) throw new Error("Invalid artifact listing");
    artifacts.push(...batch.artifacts);
    if (batch.artifacts.length < 100) break;
  }
  const candidates = artifacts.filter((item) => item.name === artifactName);
  if (candidates.length !== 1 || candidates[0].expired || candidates[0].workflow_run?.id !== positiveId(runId) || candidates[0].workflow_run?.head_sha !== source) throw new Error("Expected one unexpired candidate artifact belonging to the verified run and source");
  return { repo, runId: positiveId(runId), runAttempt, source, tree, releaseSha, artifactId: positiveId(candidates[0].id) };
}
async function assertRefs(selected, api, tag, tagMustExist) {
  const prefix = `/repos/${selected.repo}`;
  const dev = await api("GET", `${prefix}/git/ref/heads/dev`);
  const main = await api("GET", `${prefix}/git/ref/heads/main`);
  if (sha(dev.object?.sha) !== selected.source || sha(main.object?.sha) !== selected.releaseSha) throw new Error("Release references moved; refusing promotion");
  const existing = await api("GET", `${prefix}/git/ref/tags/${tag}`, undefined, { optional: true });
  if (existing && (existing.object?.type !== "commit" || existing.object.sha !== selected.releaseSha)) throw new Error("Remote stable tag target changed");
  if (tagMustExist && !existing) throw new Error("Remote stable tag disappeared");
  return existing;
}
export async function uploadAsset(token, url, name, bytes, fetchImpl = fetch) {
  const target = new URL(url.replace(/\{.*\}$/, ""));
  if (target.origin !== "https://uploads.github.com" || target.username || target.password || target.search || target.hash || !/^\/repos\/[^/]+\/[^/]+\/releases\/[1-9][0-9]*\/assets$/.test(target.pathname)) throw new Error("Unexpected release upload endpoint");
  target.searchParams.set("name", name);
  const response = await fetchImpl(target, { method: "POST", headers: { Authorization: `Bearer ${token}`, Accept: "application/vnd.github+json", "Content-Type": "application/octet-stream", "X-GitHub-Api-Version": "2022-11-28" }, body: bytes, redirect: "error" });
  if (!response.ok) throw new Error(`Release asset upload failed (${response.status})`);
  return response.json();
}
async function releasesForTag(prefix, tag, api) {
  const matches = [];
  // The release-by-tag endpoint is not a reliable draft lookup; authenticated
  // release listings include unpublished drafts and must be fully paginated.
  for (let page = 1; ; page++) {
    const batch = await api("GET", `${prefix}/releases?per_page=100&page=${page}`);
    if (!Array.isArray(batch)) throw new Error("Invalid release listing");
    matches.push(...batch.filter((release) => release.tag_name === tag));
    if (batch.length < 100) break;
  }
  if (matches.length > 1) throw new Error("Ambiguous releases for stable tag; manual recovery required");
  return matches[0];
}
function assertDraft(release, expected, repo, id = release?.id) {
  positiveId(id);
  if (!release || release.id !== id || release.draft !== true || release.published_at !== null ||
      release.prerelease !== false || Object.entries(expected).some(([key, value]) => release[key] !== value))
    throw new Error("Release is not the matching unpublished candidate draft; manual recovery required");
  const uploadUrl = `https://uploads.github.com/repos/${repo}/releases/${id}/assets{?name,label}`;
  if (release.upload_url !== uploadUrl) throw new Error("Release upload endpoint does not match this repository and draft");
}
async function verifyRemoteAssets(prefix, releaseId, manifest, names, api, complete = false) {
  const seen = new Set();
  for (let page = 1; ; page++) {
    const assets = await api("GET", `${prefix}/releases/${releaseId}/assets?per_page=100&page=${page}`);
    if (!Array.isArray(assets)) throw new Error("Invalid release asset listing");
    for (const asset of assets) {
      if (!names.includes(asset.name) || seen.has(asset.name)) throw new Error("Remote release asset inventory mismatch; manual recovery required");
      seen.add(asset.name);
      if (asset.state !== "uploaded" || asset.size !== manifest.assets[asset.name].size) throw new Error("Remote release asset size/state mismatch; manual recovery required");
      const bytes = await api("GET", `${prefix}/releases/assets/${positiveId(asset.id)}`, undefined, { binary: true });
      if (digest(bytes) !== manifest.assets[asset.name].sha256) throw new Error("Remote release bytes differ from the tested candidate; manual recovery required");
    }
    if (assets.length < 100) break;
  }
  if (complete && seen.size !== names.length) throw new Error("Remote release asset inventory mismatch");
  return seen;
}
export async function promoteCandidate(options, api, upload, { base = root } = {}) {
  const selected = await selectCandidate(options, api);
  if (selected.artifactId !== positiveId(options.artifactId)) throw new Error("Selected artifact changed after download");
  const candidate = validateCandidate(options.directory, selected, { base });
  const { tag, names, manifest, directory } = candidate;
  const prefix = `/repos/${selected.repo}`;
  const existingTag = await assertRefs(selected, api, tag, false);
  let release = await releasesForTag(prefix, tag, api);
  const expected = {
    tag_name: tag, target_commitish: selected.releaseSha, name: `Rift ${tag}`, draft: true, prerelease: false,
    body: `Windows x64. Candidate source ${selected.source}; source tree ${selected.tree}; release commit ${selected.releaseSha}; Quality run ${selected.runId} attempt ${selected.runAttempt}; artifact ${selected.artifactId}; manifest SHA-256 ${digest(readFileSync(path.join(options.directory, "manifest.json")))}. Review the installer, signature, latest.json and matching application PDB provenance before publishing.`,
  };
  if (release) {
    assertDraft(release, expected, selected.repo);
    if (!existingTag) throw new Error("Candidate draft has no stable tag; manual recovery required");
  } else {
    if (!existingTag) {
      // createRef fails on collisions; never force-update, move or delete a tag.
      await api("POST", `${prefix}/git/refs`, { ref: `refs/tags/${tag}`, sha: selected.releaseSha });
    }
    await assertRefs(selected, api, tag, true);
    release = await api("POST", `${prefix}/releases`, { ...expected, generate_release_notes: false });
    assertDraft(release, expected, selected.repo);
  }
  const assertRemoteDraft = async () => {
    await assertRefs(selected, api, tag, true);
    assertDraft(await api("GET", `${prefix}/releases/${release.id}`), expected, selected.repo, release.id);
  };
  await assertRemoteDraft();
  // A rerun may only keep already uploaded bytes after downloading and verifying
  // them. No replacement/deletion API is used, including for incomplete uploads.
  const uploaded = await verifyRemoteAssets(prefix, release.id, manifest, names, api);
  for (const name of names) {
    if (uploaded.has(name)) continue;
    await assertRemoteDraft();
    const bytes = readFileSync(path.join(directory, name));
    if (bytes.length !== manifest.assets[name].size || digest(bytes) !== manifest.assets[name].sha256) throw new Error("Candidate bytes changed before upload");
    await upload(release.upload_url, name, bytes);
  }
  await assertRemoteDraft();
  await verifyRemoteAssets(prefix, release.id, manifest, names, api, true);
  await assertRemoteDraft();
  return { tag, releaseId: release.id, source: selected.source, release: selected.releaseSha };
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const [command, directory, ...extra] = process.argv.slice(2);
    if (extra.length) throw new Error("Unexpected arguments");
    if (command === "create" && directory) {
      if (process.env.GITHUB_EVENT_NAME !== "workflow_dispatch" || process.env.GITHUB_REF !== "refs/heads/dev") throw new Error("Signed candidates require a manual dev run");
      const git = (ref) => execFileSync("git", ["rev-parse", ref], { cwd: root, encoding: "utf8" }).trim();
      createCandidate(directory, { source: process.env.SOURCE_SHA, tree: git("HEAD^{tree}"), actualSource: git("HEAD"), actualTree: git("HEAD^{tree}"), repo: process.env.GITHUB_REPOSITORY, runId: process.env.GITHUB_RUN_ID, runAttempt: process.env.GITHUB_RUN_ATTEMPT });
    } else if ((command === "select" && !directory) || (command === "promote" && directory)) {
      const options = { repo: process.env.GITHUB_REPOSITORY, runId: process.env.CANDIDATE_RUN_ID, releaseSha: process.env.GITHUB_SHA, eventName: process.env.GITHUB_EVENT_NAME, ref: process.env.GITHUB_REF, directory, artifactId: process.env.CANDIDATE_ARTIFACT_ID };
      const token = process.env.GITHUB_TOKEN;
      const api = githubApi(token);
      if (command === "select") {
        const selected = await selectCandidate(options, api);
        appendFileSync(process.env.GITHUB_OUTPUT, `artifact-id=${selected.artifactId}\nsource=${selected.source}\n`);
      } else {
        const promoted = await promoteCandidate(options, api, (url, name, bytes) => uploadAsset(token, url, name, bytes));
        console.log(`Draft ${promoted.tag} ready; remote inventory and candidate bytes verified.`);
      }
    } else throw new Error("Usage: candidate-release.mjs create <candidate-dir> | select | promote <candidate-dir>");
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
