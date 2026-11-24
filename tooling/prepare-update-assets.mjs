#!/usr/bin/env node
// Prepare signed Windows updater files locally; publication belongs to the workflow.
import { copyFileSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { assetUrl, checkVersions, installerFiles, validateAssets, releaseAssets, verifySignature } from "./release-check.mjs";

const root = path.resolve(import.meta.dirname, "..");
export function prepareUpdateAssets(tag, bundle, output, { base = root, repo = process.env.GITHUB_REPOSITORY, now = new Date() } = {}) {
  const version = checkVersions(tag, base);
  const { installer: original, data, signature } = installerFiles(bundle, version, false);
  const { installer, manifest: manifestName } = releaseAssets(version);
  const config = JSON.parse(readFileSync(path.join(base, "desktop/src-tauri/tauri.conf.json"), "utf8"));
  if (verifySignature(data, signature, config.plugins.updater.pubkey) !== version)
    throw new Error("Signed installer version does not match release tag");
  const manifest = { version, notes: `Rift ${version}`, pub_date: now.toISOString(),
    platforms: { "windows-x86_64": { signature, url: assetUrl(repo, tag, installer) } } };
  // mkdir without recursive/exist_ok deliberately refuses stale output directories.
  mkdirSync(output);
  copyFileSync(path.join(bundle, original), path.join(output, installer));
  copyFileSync(path.join(bundle, `${original}.sig`), path.join(output, `${installer}.sig`));
  writeFileSync(path.join(output, manifestName), `${JSON.stringify(manifest, null, 2)}\n`);
  return validateAssets(tag, output, { base, repo });
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const [tag, bundle, output, ...extra] = process.argv.slice(2);
    if (!tag || !bundle || !output || extra.length)
      throw new Error("Usage: prepare-update-assets.mjs <tag> <nsis-dir> <new-output-dir>");
    console.log(prepareUpdateAssets(tag, bundle, output).join("\n"));
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
