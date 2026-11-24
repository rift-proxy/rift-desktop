import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';

// Restrictive licenses require review even behind OR alternatives.
// The exact reviewed r-efi package/version/expression is the only exception.
export const restricted = new Set(['GPL-1.0', 'GPL-2.0', 'GPL-3.0', 'LGPL-2.0', 'LGPL-2.1', 'LGPL-3.0', 'AGPL-1.0', 'AGPL-3.0', 'SSPL-1.0', 'BUSL-1.1', 'EUPL-1.1', 'EUPL-1.2', 'CC-BY-NC-4.0', 'CC-BY-SA-4.0']);

export function checkLicense(name, version, expression, parse) {
  if (typeof expression !== 'string' || !expression.trim()) throw new Error(`${name}@${version}: missing license; review required`);
  let ast;
  // Older Cargo manifests use slash-separated alternatives instead of SPDX OR.
  // Normalize that documented Cargo spelling; still inspect every restricted branch.
  try { ast = parse(expression.replaceAll('/', ' OR ')); }
  catch { throw new Error(`${name}@${version}: invalid/unknown SPDX license; review required`); }
  const ids = [];
  function visit(node) {
    if (node.license) ids.push(node.license.replace(/-(only|or-later)$/, '').replace(/\+$/, ''));
    else if (node.left && node.right) { visit(node.left); visit(node.right); }
    else throw new Error(`${name}@${version}: invalid SPDX expression`);
  }
  visit(ast);
  if (name === 'r-efi' && ['5.3.0', '6.0.0'].includes(version) && expression === 'MIT OR Apache-2.0 OR LGPL-2.1-or-later') return;
  if (ids.some(id => restricted.has(id))) throw new Error(`${name}@${version}: restrictive license ${expression}`);
}

export function cargoInventory(metadata, manifestPath) {
  if (!Array.isArray(metadata.packages) || !Array.isArray(metadata.resolve?.nodes)) throw new Error('Cargo metadata has no resolved dependency graph');
  const root = metadata.packages.find(pkg => resolve(pkg.manifest_path) === resolve(manifestPath));
  if (!root) throw new Error('Requested Cargo package is missing from metadata');
  const packages = new Map(metadata.packages.map(pkg => [pkg.id, pkg]));
  const nodes = new Map(metadata.resolve.nodes.map(node => [node.id, node]));
  const seen = new Set();
  const queue = [root.id];
  while (queue.length) {
    const id = queue.pop();
    if (seen.has(id)) continue;
    seen.add(id);
    const node = nodes.get(id);
    if (!node || !packages.has(id)) throw new Error('Cargo dependency graph is incomplete');
    queue.push(...node.dependencies);
  }
  const inventory = [...seen].filter(id => id !== root.id).map(id => {
    const pkg = packages.get(id);
    return { name: pkg.name, version: pkg.version, license: pkg.license };
  }).sort((a, b) => `${a.name}@${a.version}`.localeCompare(`${b.name}@${b.version}`));
  if (!inventory.length) throw new Error('Cargo dependency inventory is empty');
  return inventory;
}

export function frontendInventory(listing, frontend) {
  if (!listing || typeof listing !== 'object' || Array.isArray(listing)) throw new Error('Invalid pnpm license inventory');
  const inventory = [];
  for (const [license, packages] of Object.entries(listing)) {
    if (!Array.isArray(packages)) throw new Error('Invalid pnpm license group');
    for (const pkg of packages) {
      if (typeof pkg.name !== 'string' || !Array.isArray(pkg.versions) || !pkg.versions.length) throw new Error('Invalid pnpm package inventory');
      for (const version of pkg.versions) {
        if (typeof version !== 'string') throw new Error('Invalid pnpm package version');
        if (pkg.name === frontend.name && version === frontend.version) continue;
        inventory.push({ name: pkg.name, version, license });
      }
    }
  }
  if (!inventory.length) throw new Error('Frontend license inventory is empty');
  return inventory;
}

function command(program, args) {
  const result = spawnSync(program, args, { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
  if (result.error || result.status !== 0) throw new Error(`${program} failed; a complete license inventory is required`);
  return JSON.parse(result.stdout);
}

export function run(root = '.', tools = process.env.RIFT_SECURITY_TOOLS) {
  if (!tools) throw new Error('RIFT_SECURITY_TOOLS must name the pinned security tools installation');
  const require = createRequire(join(resolve(tools), 'package.json'));
  const parse = require('spdx-expression-parse');
  const reports = resolve(root, 'reports/security/licenses');
  mkdirSync(reports, { recursive: true });
  const failures = [];
  for (const [name, manifest] of [['core', 'core/Cargo.toml'], ['shell', 'desktop/src-tauri/Cargo.toml'], ['fuzz', 'fuzz/Cargo.toml']]) {
    const manifestPath = resolve(root, manifest);
    // Explicit stable override avoids the fuzz directory's moving nightly; no
    // compilation or build scripts run during cargo metadata.
    const metadata = command('cargo', ['+1.98.1', 'metadata', '--locked', '--all-features', '--format-version', '1', '--manifest-path', manifestPath]);
    const inventory = cargoInventory(metadata, manifestPath);
    writeFileSync(join(reports, `${name}.json`), `${JSON.stringify(inventory, null, 2)}\n`);
    for (const pkg of inventory) {
      try { checkLicense(pkg.name, pkg.version, pkg.license, parse); }
      catch (error) { failures.push(`${name}: ${error.message}`); }
    }
  }
  const frontend = JSON.parse(readFileSync(resolve(root, 'desktop/package.json'), 'utf8'));
  // pnpm reads its own complete installed lock graph, including transitive/dev
  // packages. read-installed based npm scanners can miss pnpm's virtual store.
  const listing = command('pnpm', ['--dir', resolve(root, 'desktop'), 'licenses', 'list', '--json']);
  const inventory = frontendInventory(listing, frontend);
  writeFileSync(join(reports, 'frontend.json'), `${JSON.stringify(inventory, null, 2)}\n`);
  for (const pkg of inventory) {
    try { checkLicense(pkg.name, pkg.version, pkg.license, parse); }
    catch (error) { failures.push(`frontend: ${error.message}`); }
  }
  if (failures.length) throw new Error(failures.join('\n'));
  console.log('License policy passed for Rust core, shell, fuzz and frontend dependencies.');
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try { run(); } catch (error) { console.error(error.message); process.exitCode = 1; }
}
