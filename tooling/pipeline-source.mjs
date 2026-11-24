#!/usr/bin/env node
import { appendFileSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { repository } from "./release-check.mjs";

export function sha(value) {
  if (typeof value !== "string" || value.length !== 40 || !/^[a-f0-9]{40}$/.test(value)) throw new Error("Expected an immutable Git commit SHA");
  return value;
}
export function positiveId(value) {
  if (!["string", "number"].includes(typeof value) || String(value) !== String(value).trim() || !/^[1-9][0-9]*$/.test(String(value)) || !Number.isSafeInteger(Number(value))) throw new Error("Expected a positive safe integer ID");
  return Number(value);
}
export function githubApi(token, { fetchImpl = fetch } = {}) {
  if (!token) throw new Error("Missing GitHub API token");
  return async function request(method, endpoint, body, { optional = false, binary = false } = {}) {
    if (!endpoint.startsWith("/repos/")) throw new Error("Unexpected GitHub API path");
    const response = await fetchImpl(`https://api.github.com${endpoint}`, {
      method,
      headers: { Authorization: `Bearer ${token}`, Accept: binary ? "application/octet-stream" : "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28", ...(body === undefined ? {} : { "Content-Type": "application/json" }) },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    });
    if (optional && response.status === 404) return null;
    if (!response.ok) throw new Error(`GitHub API request failed (${response.status})`);
    return binary ? Buffer.from(await response.arrayBuffer()) : response.json();
  };
}
export async function resolveSource({ eventName, ref, source, event, repo }, api) {
  repository(repo);
  sha(source);
  if (eventName === "pull_request") {
    const pr = event.pull_request;
    if (!pr || !["dev", "main"].includes(pr.base?.ref) || !/^refs\/pull\/[1-9][0-9]*\/merge$/.test(ref) || pr.base.repo?.full_name !== repo)
      throw new Error("Expected a pull request merge ref targeting this repository's dev or main");
    return { source, baseline: sha(pr.base.sha), full: false };
  }
  if (eventName !== "workflow_dispatch" || ref !== "refs/heads/dev") throw new Error("Full Quality must be dispatched on dev");
  const dev = await api("GET", `/repos/${repo}/git/ref/heads/dev`);
  if (sha(dev.object?.sha) !== source) throw new Error("dev moved after dispatch; dispatch Quality again");
  const main = await api("GET", `/repos/${repo}/git/ref/heads/main`);
  return { source, baseline: sha(main.object?.sha), full: true };
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    if (process.argv.length !== 2) throw new Error("Usage: pipeline-source.mjs");
    const resolved = await resolveSource({ eventName: process.env.GITHUB_EVENT_NAME, ref: process.env.GITHUB_REF, source: process.env.GITHUB_SHA, event: JSON.parse(readFileSync(process.env.GITHUB_EVENT_PATH, "utf8")), repo: process.env.GITHUB_REPOSITORY }, githubApi(process.env.GITHUB_TOKEN));
    appendFileSync(process.env.GITHUB_OUTPUT, `source=${resolved.source}\nbaseline=${resolved.baseline}\nfull=${resolved.full}\n`);
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
