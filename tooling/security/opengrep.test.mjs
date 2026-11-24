import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { assess } from './opengrep.mjs';

const position = { line: 1, col: 1, offset: 0 };
const finding = severity => ({
  check_id: 'fixture.rule', path: 'fixture.js', start: position,
  end: { line: 1, col: 2, offset: 1 }, extra: { severity, message: 'Fixture finding' },
});
const diagnostic = level => ({ level, code: 3, type: 'ParseError', message: 'Fixture diagnostic' });
const report = (severities, errors = []) => ({
  version: '2.0.0', paths: { scanned: ['fixture.js'] },
  results: severities.map(finding), errors,
});

test('WARNING/INFO findings and warning-level analysis remain nonblocking', () => {
  const result = assess(report(['INFO', 'WARNING'], [diagnostic('warn'), diagnostic('info')]));
  assert.deepEqual(result, {
    findings: { ERROR: 0, WARNING: 1, INFO: 1 },
    diagnostics: { error: 0, warn: 1, info: 1 },
    failed: false,
  });
});

test('an ERROR blocks even when preceded or followed by nonblocking findings', () => {
  for (const severities of [
    ['ERROR', 'INFO', 'WARNING'],
    ['INFO', 'ERROR', 'WARNING'],
    ['INFO', 'WARNING', 'ERROR'],
  ]) {
    const result = assess(report(severities));
    assert.equal(result.failed, true);
    assert.equal(result.findings.ERROR, 1);
  }
});

test('error-level analysis blocks even without security findings', () => {
  const result = assess(report([], [diagnostic('warn'), diagnostic('error')]));
  assert.equal(result.failed, true);
  assert.equal(result.diagnostics.error, 1);
});

test('missing or unknown schema cannot silently turn the gate green', () => {
  for (const value of [null, {}, { results: [] }, { errors: [] },
    { results: {}, errors: [] }, { results: [], errors: {} },
    { results: [null], errors: [] }, report(['NOTICE']), report(['warning']),
    report([['WARNING']]), report([], [null]), report([], [{}]),
    report([], [diagnostic('fatal')]), report([], [diagnostic(['warn'])])]) {
    assert.throws(() => assess(value), /Invalid OpenGrep/);
  }
});

test('incomplete reports and finding locations fail closed', () => {
  const valid = report(['WARNING']);
  for (const field of ['version', 'paths', 'results', 'errors']) {
    const incomplete = structuredClone(valid);
    delete incomplete[field];
    assert.throws(() => assess(incomplete), /Invalid OpenGrep/, field);
  }
  for (const field of ['check_id', 'path', 'start', 'end']) {
    const incomplete = structuredClone(valid);
    delete incomplete.results[0][field];
    assert.throws(() => assess(incomplete), /Invalid OpenGrep/, field);
  }
  for (const invalid of [
    { results: [{ extra: { severity: 'WARNING' } }], errors: [] },
    { ...valid, version: '' },
    { ...valid, paths: { scanned: 'fixture.js' } },
    { ...valid, paths: { scanned: [null] } },
    { ...valid, results: [{ ...finding('WARNING'), path: '' }] },
    { ...valid, results: [{ ...finding('WARNING'), start: { ...position, line: 0 } }] },
    { ...valid, results: [{ ...finding('WARNING'), end: { ...position, offset: -1 } }] },
    report([], [{ level: 'warn' }]),
  ]) assert.throws(() => assess(invalid), /Invalid OpenGrep/);
});

test('CLI enforces severity and keeps the original full report intact', t => {
  const directory = mkdtempSync(join(tmpdir(), 'rift-opengrep-gate-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const path = join(directory, 'report.json');
  const cli = fileURLToPath(new URL('./opengrep.mjs', import.meta.url));
  for (const [input, expected] of [
    [report(['WARNING', 'INFO'], [diagnostic('warn')]), 0],
    [report(['WARNING', 'ERROR', 'INFO']), 1],
    [report([], [diagnostic('error')]), 1],
  ]) {
    const original = JSON.stringify(input);
    writeFileSync(path, original);
    const result = spawnSync(process.execPath, [cli, path], { encoding: 'utf8' });
    assert.equal(result.status, expected, result.stderr);
    assert.match(result.stdout, /OpenGrep findings: ERROR=/);
    assert.equal(readFileSync(path, 'utf8'), original);
  }
  writeFileSync(path, JSON.stringify({ results: [{ extra: { severity: 'WARNING' } }], errors: [] }));
  const incomplete = spawnSync(process.execPath, [cli, path], { encoding: 'utf8' });
  assert.equal(incomplete.status, 1);
  assert.match(incomplete.stderr, /OpenGrep gate failed/);
  writeFileSync(path, '{');
  assert.equal(spawnSync(process.execPath, [cli, path]).status, 1);
  rmSync(path);
  assert.equal(spawnSync(process.execPath, [cli, path]).status, 1);
});

test('public CLI output shows locations without source, messages or machine paths', t => {
  const directory = mkdtempSync(join(tmpdir(), 'rift-opengrep-privacy-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const path = join(directory, 'report.json');
  const input = report([]);
  input.results = [
    { ...finding('WARNING'), check_id: 'fixture.warning', path: 'core/src/fixture.rs',
      start: { line: 17, col: 1, offset: 100 }, end: { line: 17, col: 2, offset: 101 },
      extra: { severity: 'WARNING', message: 'private-message-canary', lines: 'private-source-canary' } },
    { ...finding('INFO'), check_id: 'fixture.info', path: 'C:\\Users\\user\\fixture.rs',
      extra: { severity: 'INFO', message: 'private-message-canary' } },
  ];
  input.errors = [{ ...diagnostic('warn'), type: 'PartialParsing', path: '/home/user/fixture.rs',
    message: 'private-diagnostic-canary' }];
  writeFileSync(path, JSON.stringify(input));
  const cli = fileURLToPath(new URL('./opengrep.mjs', import.meta.url));
  const result = spawnSync(process.execPath, [cli, path], { encoding: 'utf8' });
  assert.equal(result.status, 0, result.stderr);
  const output = result.stdout + result.stderr;
  for (const hidden of ['private-message-canary', 'private-source-canary',
    'private-diagnostic-canary', '/home/user', 'C:\\Users']) {
    assert.equal(output.includes(hidden), false, hidden);
  }
  const rows = result.stdout.split('\n').filter(line => line.startsWith('{')).map(line => JSON.parse(line));
  assert.ok(rows.some(row => row.severity === 'WARNING' && row.rule === 'fixture.warning' &&
    row.path === 'core/src/fixture.rs' && row.line === 17));
  assert.ok(rows.some(row => row.severity === 'INFO'));
  assert.ok(rows.some(row => row.analysis === 'warn' && row.kind === 'PartialParsing'));
  writeFileSync(path, '{"source":"private-source-canary"');
  const broken = spawnSync(process.execPath, [cli, path], { encoding: 'utf8' });
  assert.equal(broken.status, 1);
  assert.equal((broken.stdout + broken.stderr).includes('private-source-canary'), false);
});
