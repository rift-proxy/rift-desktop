import assert from 'node:assert/strict';
import test from 'node:test';
import { createRequire } from 'node:module';
import { resolve, join } from 'node:path';
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
