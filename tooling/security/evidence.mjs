import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

export function publicLockfiles(tracked) {
  const locks = tracked.filter(path => /(^|\/)(Cargo\.lock|pnpm-lock\.yaml|package-lock\.json|yarn\.lock)$/.test(path));
  for (const required of ['Cargo.lock', 'fuzz/Cargo.lock', 'desktop/pnpm-lock.yaml']) {
    if (!locks.includes(required)) throw new Error(`Missing public lockfile: ${required}`);
  }
  return [...new Set(locks)].sort();
}

export function validateBom(bom, lockfile) {
  if (bom?.bomFormat !== 'CycloneDX' || typeof bom.specVersion !== 'string' || !Array.isArray(bom.components) || !bom.components.length) throw new Error(`Empty or invalid CycloneDX inventory for ${lockfile}`);
  const ecosystem = lockfile.endsWith('Cargo.lock') ? 'cargo' : 'npm';
  if (!bom.components.some(component => typeof component.purl === 'string' && component.purl.startsWith(`pkg:${ecosystem}/`))) throw new Error(`No ${ecosystem} packages in SBOM for ${lockfile}`);
  return bom.components.length;
}

function command(program, args, cwd) {
  const run = spawnSync(program, args, { cwd, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
  if (run.error || run.status !== 0) throw new Error(`${program} failed; SBOM evidence is incomplete`);
  return run.stdout.trim();
}
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');

export function generate(root = '.') {
  root = resolve(root);
  const sha = command('git', ['rev-parse', 'HEAD'], root);
  if (!/^[a-f0-9]{40}$/.test(sha)) throw new Error('Expected immutable source SHA');
  const locks = publicLockfiles(command('git', ['ls-files', '-z'], root).split('\0').filter(Boolean));
  const out = resolve(root, 'reports/security/sbom');
  mkdirSync(out, { recursive: true });
  const inventories = [];
  for (const lockfile of locks) {
    const name = `${lockfile.replaceAll('/', '__')}.cdx.json`;
    const destination = join(out, name);
    // Inventory-only CycloneDX does not need a vulnerability database or repeat gate12.
    // Scan every lock separately so nested Cargo/pnpm inventories cannot disappear.
    command('trivy', ['fs', '--skip-version-check', '--no-progress', '--include-dev-deps', '--format', 'cyclonedx', '--output', destination, resolve(root, lockfile)], root);
    const bytes = readFileSync(destination);
    const count = validateBom(JSON.parse(bytes), lockfile);
    inventories.push({ lockfile, lockSha256: sha256(readFileSync(resolve(root, lockfile))), sbom: name, sbomSha256: sha256(bytes), components: count });
  }
  const trivy = command('trivy', ['--version'], root);
  writeFileSync(join(out, 'evidence.json'), `${JSON.stringify({ sourceSha: sha, scanner: trivy, inventories }, null, 2)}\n`);
  console.log(`CycloneDX evidence generated for ${inventories.length} public lockfiles.`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try { generate(); } catch (error) { console.error(error.message); process.exitCode = 1; }
}
