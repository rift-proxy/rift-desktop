import assert from 'node:assert/strict';
import test from 'node:test';
import { createRequire } from 'node:module';
import { resolve, join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, symlinkSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { cargoInventory, frontendInventory, checkLicense } from './licenses.mjs';

if (!process.env.RIFT_SECURITY_TOOLS) throw new Error('Set RIFT_SECURITY_TOOLS to the pinned security tools installation');
const parse = createRequire(join(resolve(process.env.RIFT_SECURITY_TOOLS), 'package.json'))('spdx-expression-parse');

test('restrictive SPDX choices cannot hide behind OR, exceptions or suffixes', () => {
  for (const license of ['MIT OR GPL-3.0-only', 'LGPL-2.1-or-later', 'GPL-2.0+', 'AGPL-1.0-only', 'SSPL-1.0', 'BUSL-1.1', 'EUPL-1.2', 'CC-BY-NC-4.0', 'CC-BY-SA-4.0', 'GPL-2.0-only WITH Classpath-exception-2.0']) {
    assert.throws(() => checkLicense('dep', '1.0.0', license, parse), /restrictive/);
  }
  for (const license of ['MIT OR Apache-2.0', '(MIT AND BSD-3-Clause)', 'MPL-2.0']) {
    assert.doesNotThrow(() => checkLicense('dep', '1.0.0', license, parse));
  }
});
test('GPL 1.0 spellings and alternatives require restrictive-license review', () => {
  for (const license of ['GPL-1.0', 'GPL-1.0-only', 'GPL-1.0-or-later', 'GPL-1.0+', 'MIT OR GPL-1.0-only', 'MIT/GPL-1.0-or-later']) {
    assert.throws(() => checkLicense('dep', '1.0.0', license, parse), /restrictive license/);
  }
});
test('legacy Cargo slash alternatives retain the same restrictive-license policy', () => {
  for (const license of ['MIT/Apache-2.0', 'MIT / Apache-2.0']) {
    assert.doesNotThrow(() => checkLicense('fnv', '1.0.7', license, parse));
  }
  assert.throws(() => checkLicense('dep', '1.0.0', 'MIT/GPL-3.0-only', parse), /restrictive/);
  for (const license of ['MIT//Apache-2.0', 'https://example.invalid/LICENSE']) {
    assert.throws(() => checkLicense('dep', '1.0.0', license, parse));
  }
});
test('only the exact reviewed r-efi package/version/expression is exempt', () => {
  const expression = 'MIT OR Apache-2.0 OR LGPL-2.1-or-later';
  for (const version of ['5.3.0', '6.0.0']) assert.doesNotThrow(() => checkLicense('r-efi', version, expression, parse));
  assert.throws(() => checkLicense('other', '6.0.0', expression, parse));
  assert.throws(() => checkLicense('r-efi', '6.0.1', expression, parse));
  assert.throws(() => checkLicense('r-efi', '6.0.0', 'LGPL-2.1-or-later', parse));
});
test('unknown, missing and malformed licenses require review', () => {
  for (const license of [null, '', 'UNKNOWN', 'custom license', 'MIT OR']) assert.throws(() => checkLicense('dep', '1', license, parse));
});
test('Cargo inventories include transitive dependencies without unrelated workspace packages', () => {
  const metadata = {
    packages: [
      { id: 'core', name: 'core', version: '1', manifest_path: '/public/core/Cargo.toml', license: 'MIT' },
      { id: 'direct', name: 'direct', version: '1', manifest_path: '/registry/direct/Cargo.toml', license: 'Apache-2.0' },
      { id: 'indirect', name: 'indirect', version: '2', manifest_path: '/registry/indirect/Cargo.toml', license: 'GPL-3.0-only' },
      { id: 'shell', name: 'shell', version: '1', manifest_path: '/public/desktop/src-tauri/Cargo.toml', license: 'MIT' },
    ],
    resolve: { nodes: [
      { id: 'core', dependencies: ['direct'] }, { id: 'direct', dependencies: ['indirect'] },
      { id: 'indirect', dependencies: ['direct'] }, { id: 'shell', dependencies: [] },
    ] },
  };
  assert.deepEqual(cargoInventory(metadata, '/public/core/Cargo.toml').map(pkg => pkg.name), ['direct', 'indirect']);
  assert.throws(() => cargoInventory(metadata, '/public/missing/Cargo.toml'));
  metadata.resolve.nodes.pop();
  assert.throws(() => cargoInventory(metadata, '/public/desktop/src-tauri/Cargo.toml'), /incomplete/);
});

test('pnpm inventory retains every grouped transitive version and restrictive license', () => {
  const frontend = { name: '@rift/desktop', version: '1' };
  const listing = {
    MIT: [{ name: '@rift/desktop', versions: ['1'] }, { name: 'transitive', versions: ['1.0.0', '2.0.0'] }],
    'GPL-3.0-only': [{ name: 'restricted', versions: ['3.0.0'] }],
  };
  const inventory = frontendInventory(listing, frontend);
  assert.deepEqual(inventory.map(pkg => `${pkg.name}@${pkg.version}`), ['transitive@1.0.0', 'transitive@2.0.0', 'restricted@3.0.0']);
  assert.throws(() => checkLicense(inventory[2].name, inventory[2].version, inventory[2].license, parse), /restrictive/);
  for (const malformed of [{}, { MIT: {} }, { MIT: [{ name: 'dep', versions: [] }] }]) {
    assert.throws(() => frontendInventory(malformed, frontend));
  }
});

const helper = fileURLToPath(new URL('./licenses.mjs', import.meta.url));
const privateDiagnostic = '/private/store/token=do-not-publish https://user:password@example.invalid/secret';

function licenseCli(t, pnpm = {}, cargo) {
  const root = mkdtempSync(join(tmpdir(), 'rift-license-fixture-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const bin = join(root, 'bin');
  mkdirSync(bin);
  mkdirSync(join(root, 'desktop'));
  writeFileSync(join(root, 'desktop/package.json'), JSON.stringify({ name: '@rift/desktop', version: '1' }));
  symlinkSync(process.execPath, join(bin, 'node'));
  const executable = `#!${process.execPath}
const { readFileSync, writeFileSync } = require('node:fs');
const { join, dirname, basename } = require('node:path');
const root = dirname(dirname(process.argv[1]));
const config = JSON.parse(readFileSync(join(root, 'fixture.json'), 'utf8'));
const program = basename(process.argv[1]);
writeFileSync(join(root, program + '-args.json'), JSON.stringify(process.argv.slice(2)));
const behavior = config[program];
if (behavior) {
  if (behavior.signal) process.kill(process.pid, behavior.signal);
  if (behavior.bytes) process.stdout.write('x'.repeat(behavior.bytes));
  else process.stdout.write(behavior.stdout ?? '');
  process.stderr.write(behavior.stderr ?? '');
  process.exitCode = behavior.exit ?? 0;
} else {
  const manifest = process.argv.at(-1);
  process.stdout.write(JSON.stringify({
    packages: [
      { id: 'root', name: 'root', version: '1', manifest_path: manifest, license: 'MIT' },
      { id: 'dep', name: 'cargo-dependency', version: '2', license: 'MIT' }
    ],
    resolve: { nodes: [{ id: 'root', dependencies: ['dep'] }, { id: 'dep', dependencies: [] }] }
  }));
}
`;
  writeFileSync(join(bin, 'cargo'), executable, { mode: 0o700 });
  if (!pnpm.missing) writeFileSync(join(bin, 'pnpm'), executable, { mode: pnpm.denied ? 0o600 : 0o700 });
  writeFileSync(join(root, 'fixture.json'), JSON.stringify({ pnpm, cargo }));
  const result = spawnSync(process.execPath, [helper], {
    cwd: root, encoding: 'utf8',
    env: { PATH: bin, RIFT_SECURITY_TOOLS: resolve(process.env.RIFT_SECURITY_TOOLS) },
  });
  assert.ifError(result.error);
  const output = `${result.stdout}${result.stderr}`;
  for (const forbidden of [privateDiagnostic, root, 'do-not-publish', 'password', 'example.invalid']) assert.ok(!output.includes(forbidden), output);
  return { ...result, root, output };
}

test('CLI preserves production, dev, optional and every transitive version in frontend evidence', t => {
  const listing = { MIT: [
    { name: '@rift/desktop', versions: ['1'] },
    { name: 'production', versions: ['1.0.0'] },
    { name: 'transitive', versions: ['1.0.0', '2.0.0'] },
    { name: 'dev', versions: ['3.0.0'] },
    { name: 'optional', versions: ['4.0.0'] },
  ] };
  const result = licenseCli(t, { stdout: JSON.stringify(listing) });
  assert.equal(result.status, 0, result.output);
  assert.match(result.stdout, /License policy passed/);
  assert.deepEqual(JSON.parse(readFileSync(join(result.root, 'pnpm-args.json'), 'utf8')),
    ['--dir', join(result.root, 'desktop'), 'licenses', 'list', '--json']);
  assert.deepEqual(JSON.parse(readFileSync(join(result.root, 'reports/security/licenses/frontend.json'), 'utf8')), [
    { name: 'production', version: '1.0.0', license: 'MIT' },
    { name: 'transitive', version: '1.0.0', license: 'MIT' },
    { name: 'transitive', version: '2.0.0', license: 'MIT' },
    { name: 'dev', version: '3.0.0', license: 'MIT' },
    { name: 'optional', version: '4.0.0', license: 'MIT' },
  ]);
});

test('CLI exposes only bounded known pnpm inventory error metadata and recovery guidance', t => {
  for (const [code, category] of [
    ['ERR_PNPM_MISSING_PACKAGE_INDEX_FILE', 'missing-package-index'],
    ['ERR_PNPM_MISSING_PACKAGE_MANIFEST', 'missing-package-manifest'],
    ['ERR_PNPM_LICENSES_NO_LOCKFILE', 'missing-lockfile'],
    ['ERR_PNPM_UNSUPPORTED_PACKAGE_TYPE', 'unsupported-package-type'],
  ]) {
    const result = licenseCli(t, { stdout: JSON.stringify({ error: { code, message: privateDiagnostic } }), stderr: privateDiagnostic, exit: 23 });
    assert.equal(result.status, 1);
    assert.match(result.stderr, new RegExp(`pnpm inventory command failed \\(exit=23; code=${code}; category=${category}\\)`));
    assert.ok(result.stderr.length < 500);
    if (category.startsWith('missing-package')) assert.match(result.stderr, /pnpm store.*frozen frontend install/);
    for (const name of ['core', 'shell', 'fuzz']) assert.ok(existsSync(join(result.root, `reports/security/licenses/${name}.json`)));
    assert.ok(!existsSync(join(result.root, 'reports/security/licenses/frontend.json')));
  }
});

test('CLI distinguishes missing executables, permissions, termination and unknown child failures', t => {
  for (const [behavior, expected] of [
    [{ missing: true }, /pnpm inventory command failed \(code=ENOENT; category=executable-not-found\)/],
    [{ denied: true }, /code=EACCES; category=executable-not-permitted/],
    [{ signal: 'SIGTERM' }, /signal=SIGTERM; category=terminated/],
    [{ stdout: JSON.stringify({ error: { code: privateDiagnostic, message: privateDiagnostic } }), stderr: privateDiagnostic, exit: 7 }, /exit=7; category=nonzero-exit/],
    [{ stderr: `ERR_PNPM_MISSING_PACKAGE_INDEX_FILE ${privateDiagnostic}`, exit: 1 }, /code=ERR_PNPM_MISSING_PACKAGE_INDEX_FILE; category=missing-package-index/],
  ]) {
    const result = licenseCli(t, behavior);
    assert.equal(result.status, 1);
    assert.match(result.stderr, expected);
    assert.ok(!existsSync(join(result.root, 'reports/security/licenses/frontend.json')));
  }
});

test('CLI identifies a subprocess output limit instead of hiding the spawn error', t => {
  const result = licenseCli(t, { bytes: 65 * 1024 * 1024 });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /pnpm inventory command failed .*code=ENOBUFS; category=output-limit-exceeded/);
});

test('CLI separates empty output, JSON parse failures and invalid or empty inventories', t => {
  for (const [stdout, expected] of [
    [' \n', /pnpm inventory output is empty/],
    [privateDiagnostic, /pnpm inventory output is not valid JSON/],
    ['null', /Invalid pnpm license inventory/],
    ['[]', /Invalid pnpm license inventory/],
    ['{"MIT":{}}', /Invalid pnpm license group/],
    ['{"MIT":[{"name":"dep","versions":[]}]}', /Invalid pnpm package inventory/],
    ['{}', /Frontend license inventory is empty/],
    ['{"MIT":[{"name":"@rift/desktop","versions":["1"]}]}', /Frontend license inventory is empty/],
  ]) {
    const result = licenseCli(t, { stdout });
    assert.equal(result.status, 1);
    assert.match(result.stderr, expected);
    assert.ok(!existsSync(join(result.root, 'reports/security/licenses/frontend.json')));
  }
});

test('CLI rejects restrictive transitive/dev/optional licenses even alongside MIT alternatives', t => {
  const result = licenseCli(t, { stdout: JSON.stringify({
    MIT: [{ name: 'production', versions: ['1'] }],
    'MIT OR GPL-3.0-only': [{ name: 'optional-transitive', versions: ['2', '3'] }, { name: 'dev', versions: ['4'] }],
  }) });
  assert.equal(result.status, 1);
  for (const dependency of ['optional-transitive@2', 'optional-transitive@3', 'dev@4']) assert.ok(result.stderr.includes(`${dependency}: restrictive license`));
  assert.equal(JSON.parse(readFileSync(join(result.root, 'reports/security/licenses/frontend.json'), 'utf8')).length, 4);
});

test('CLI keeps cargo subprocess diagnostics sanitized too', t => {
  const result = licenseCli(t, {}, { stdout: privateDiagnostic, stderr: privateDiagnostic, exit: 9 });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /cargo inventory command failed \(exit=9; category=nonzero-exit\)/);
  assert.ok(!existsSync(join(result.root, 'pnpm-args.json')));
});
