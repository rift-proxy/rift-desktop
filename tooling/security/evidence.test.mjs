import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { delimiter, dirname, join, relative } from 'node:path';
import test from 'node:test';
import { publicLockfiles, validateBom } from './evidence.mjs';

const locks = ['Cargo.lock', 'fuzz/Cargo.lock', 'desktop/pnpm-lock.yaml'];
test('inventory covers nested public locks and any additional supported lock', () => {
  assert.deepEqual(publicLockfiles([...locks, 'example/pnpm-lock.yaml', 'core/src/lib.rs']), [...locks, 'example/pnpm-lock.yaml'].sort());
  for (const omitted of locks) assert.throws(() => publicLockfiles(locks.filter(path => path !== omitted)), /Missing public lockfile/);
});
test('empty, wrong ecosystem or non-CycloneDX reports are not evidence', () => {
  const bom = { bomFormat: 'CycloneDX', specVersion: '1.6', components: [{ purl: 'pkg:cargo/example@1.0.0' }] };
  assert.equal(validateBom(bom, 'fuzz/Cargo.lock'), 1);
  assert.throws(() => validateBom(bom, 'desktop/pnpm-lock.yaml'), /No npm packages/);
  assert.throws(() => validateBom({ ...bom, components: [] }, 'Cargo.lock'), /Empty or invalid/);
  assert.throws(() => validateBom({ ...bom, bomFormat: 'SPDX' }, 'Cargo.lock'), /Empty or invalid/);
  assert.throws(() => validateBom({}, 'Cargo.lock'));
  assert.equal(validateBom({ ...bom, components: [{ purl: 'pkg:npm/example@1.0.0' }] }, 'desktop/pnpm-lock.yaml'), 1);
});

test('generation binds provenance, tracked locks and scanner paths to the selected public root', { skip: process.platform === 'win32' }, (t) => {
  const base = mkdtempSync(join(tmpdir(), 'rift-sbom-root-'));
  t.after(() => rmSync(base, { recursive: true, force: true }));
  const source = join(base, 'source');
  const root = join(source, 'project');
  const other = join(base, 'unrelated');
  const bin = join(base, 'bin');
  const log = join(base, 'scanner.jsonl');
  const put = (file, contents) => {
    mkdirSync(dirname(file), { recursive: true });
    writeFileSync(file, contents);
  };
  const git = (cwd, ...args) => {
    const result = spawnSync('git', args, { cwd, encoding: 'utf8' });
    assert.equal(result.status, 0, result.stderr);
    return result.stdout.trim();
  };
  for (const checkout of [source, other]) {
    const publicRoot = checkout === source ? root : checkout;
    for (const lock of locks) put(join(publicRoot, lock), `${checkout === source ? 'selected' : 'unrelated'}:${lock}\n`);
    put(join(checkout, 'private', 'Cargo.lock'), 'not part of the selected public tree\n');
    git(checkout, 'init', '--quiet');
    git(checkout, 'add', '.');
    git(checkout, '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', '-c', 'commit.gpgsign=false', 'commit', '--quiet', '-m', 'isolated evidence fixture');
  }
  const sha = git(source, 'rev-parse', 'HEAD');
  assert.notEqual(sha, git(other, 'rev-parse', 'HEAD'));
  // Only Trivy is substituted: real Git checkouts and generated evidence files
  // exercise root selection without downloading a scanner/database in unit tests.
  put(join(bin, 'trivy'), `#!/usr/bin/env node
const fs = require('node:fs');
const args = process.argv.slice(2);
fs.appendFileSync(process.env.SCANNER_LOG, JSON.stringify({ cwd: process.cwd(), args }) + '\\n');
if (args[0] === '--version') {
  console.log('Trivy fixture 1.0');
} else {
  const lock = args.at(-1);
  const ecosystem = lock.endsWith('Cargo.lock') ? 'cargo' : 'npm';
  if (!fs.readFileSync(lock, 'utf8').startsWith('selected:')) process.exit(2);
  fs.writeFileSync(args[args.indexOf('--output') + 1], JSON.stringify({
    bomFormat: 'CycloneDX', specVersion: '1.6', components: [{ purl: 'pkg:' + ecosystem + '/fixture@1.0.0' }]
  }));
}
`);
  chmodSync(join(bin, 'trivy'), 0o755);
  const entry = new URL('./evidence.mjs', import.meta.url).href;
  const out = join(root, 'reports/security/sbom');
  for (const selectedRoot of [relative(other, root), root]) {
    const result = spawnSync(process.execPath, [
      '--input-type=module', '--eval', `import { generate } from ${JSON.stringify(entry)}; generate(process.argv[1]);`, selectedRoot,
    ], { cwd: other, encoding: 'utf8', env: { ...process.env, PATH: `${bin}${delimiter}${process.env.PATH}`, SCANNER_LOG: log } });
    assert.equal(result.status, 0, result.stderr);
    const evidence = JSON.parse(readFileSync(join(out, 'evidence.json'), 'utf8'));
    assert.equal(evidence.sourceSha, sha);
    assert.equal(evidence.scanner, 'Trivy fixture 1.0');
    assert.deepEqual(evidence.inventories.map(item => item.lockfile), [...locks].sort());
    for (const inventory of evidence.inventories) {
      const hash = file => createHash('sha256').update(readFileSync(file)).digest('hex');
      assert.equal(inventory.lockSha256, hash(join(root, inventory.lockfile)));
      assert.equal(inventory.sbomSha256, hash(join(out, inventory.sbom)));
      assert.equal(inventory.components, 1);
    }
    assert.ok(!existsSync(join(other, 'reports')));
    rmSync(out, { recursive: true });
  }
  const calls = readFileSync(log, 'utf8').trim().split('\n').map(line => JSON.parse(line));
  assert.equal(calls.length, 2 * (locks.length + 1));
  assert.ok(calls.every(call => call.cwd === root));
  const scannedLocks = [...locks].sort().map(lock => join(root, lock));
  assert.deepEqual(calls.filter(call => call.args[0] === 'fs').map(call => call.args.at(-1)), [...scannedLocks, ...scannedLocks]);
});
