import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { progressSnapshot, runMutations } from './mutation-progress.mjs';

const outcome = (baseline = 'Success', counts = {}) => ({
  cargo_mutants_version: '27.1.0', total_mutants: 0, caught: 0, missed: 0,
  timeout: 0, unviable: 0, success: 0, ...counts,
  outcomes: [{ scenario: 'Baseline', summary: baseline },
    ...Object.entries({ caught: 'CaughtMutant', missed: 'MissedMutant', timeout: 'Timeout',
      unviable: 'Unviable', success: 'Success' }).flatMap(([category, summary]) =>
      Array.from({ length: counts[category] ?? 0 }, (_, index) => ({
        scenario: { Mutant: { category, index } }, summary,
      })))],
});
const build = '0.5s DEBUG run{phase=Build}: start process quoted_argv=secret-canary\n';
const testing = `${build}1s DEBUG run{phase=Build}: process_status=Success\n2s DEBUG run{phase=Test}: start process quoted_argv=secret-canary\n`;

test('baseline and mutant phases distinguish completed work from the planned total', () => {
  const baseline = progressSnapshot({ ...outcome(), outcomes: [] }, 157, build, 65);
  assert.equal(baseline.stage, 'baseline');
  assert.equal(baseline.phase, 'build');
  assert.equal(baseline.completed, 0);
  assert.equal(baseline.total, 157);
  assert.equal(baseline.elapsed_seconds, 65);
  const running = progressSnapshot({ ...outcome('Success', { total_mutants: 2, caught: 1, unviable: 1 }),
    raw: 'secret-canary' }, 157, testing, 90);
  assert.equal(running.stage, 'mutants');
  assert.equal(running.phase, 'test');
  assert.equal(running.completed, 2);
  assert.equal(running.caught, 1);
  assert.equal(running.unviable, 1);
  assert.ok(!JSON.stringify(running).includes('secret-canary'));
  assert.equal(progressSnapshot(outcome(), 157, `${testing}3s DEBUG run{phase=Test}: process_status=Success\n`, 91).phase, null);
});

test('failed baseline and invalid counters cannot be reported as successful progress', () => {
  assert.equal(progressSnapshot(outcome('Failure'), 3, build, 5).stage, 'baseline-failed');
  for (const invalid of [null, { ...outcome(), cargo_mutants_version: 'unknown' },
    { ...outcome(), caught: -1 }, { ...outcome(), outcomes: null }, outcome('unknown'),
    outcome('Success', { total_mutants: 4, caught: 4 }),
    { ...outcome('Success', { total_mutants: 3, caught: 3 }), outcomes: outcome().outcomes },
    { ...outcome('Success', { total_mutants: 1, caught: 1 }), caught: 0, missed: 1 },
    { ...outcome(), outcomes: [...outcome().outcomes, ...outcome().outcomes] }]) {
    assert.throws(() => progressSnapshot(invalid, 3, build, 5));
  }
});

function fixture(t, behavior = 'success') {
  const root = mkdtempSync(path.join(os.tmpdir(), 'mutation-progress-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const command = path.join(root, 'cargo');
  const outputParent = path.join(root, 'output');
  const script = `#!${process.execPath}\nconst fs = require('node:fs'); const path = require('node:path');
const output = process.argv[process.argv.indexOf('--output') + 1];
const directory = path.join(output, 'mutants.out'); fs.mkdirSync(directory, {recursive:true});
fs.writeFileSync(path.join(directory,'mutants.json'), JSON.stringify([{}, {}, {}]));
fs.writeFileSync(path.join(directory,'debug.log'), ${JSON.stringify(build)});
console.log('secret-canary'); console.error('secret-canary');
const success = ${JSON.stringify(outcome('Success', { total_mutants: 3, caught: 3 }))};
function finish(report, code) { fs.writeFileSync(path.join(directory,'outcomes.json'), JSON.stringify(report)); process.exit(code); }
const behavior = ${JSON.stringify(behavior)};
if (behavior === 'failure') finish(${JSON.stringify(outcome('Failure'))}, 4);
else if (behavior === 'killed') process.kill(process.pid, 'SIGKILL');
else if (behavior === 'invalid') { fs.writeFileSync(path.join(directory,'outcomes.json'), '{'); process.exit(0); }
else if (behavior === 'incomplete') finish(${JSON.stringify(outcome('Success', { total_mutants: 1, caught: 1 }))}, 0);
else if (behavior === 'inconsistent') finish({ ...success, outcomes: success.outcomes.slice(0, 1) }, 0);
else if (behavior === 'signal') { process.on('SIGTERM', () => process.exit(0)); setTimeout(() => process.exit(89), 15000); }
else if (behavior === 'heartbeat') {
  const ack = path.join(output,'ack');
  fs.watch(output, () => { if (fs.existsSync(ack)) finish(success, 0); });
  if (fs.existsSync(ack)) finish(success, 0);
  setTimeout(() => process.exit(89), 15000);
} else finish(success, 0);
`;
  writeFileSync(command, script);
  chmodSync(command, 0o755);
  return { root, command, outputParent };
}

for (const [behavior, expected] of [['success', 0], ['failure', 4], ['killed', 128 + os.constants.signals.SIGKILL], ['invalid', 1], ['incomplete', 1], ['inconsistent', 1]]) {
  test(`runner preserves ${behavior} outcome without publishing child logs`, async t => {
    const config = fixture(t, behavior), records = [];
    const code = await runMutations([], { ...config, cwd: config.root, pollMs: 5,
      heartbeatMs: 10, emit: record => records.push(record) });
    assert.equal(code, expected);
    assert.equal(records.at(-1).stage, expected === 0 ? 'finished' : 'failed');
    assert.equal(records.at(-1).exit_code, expected);
    assert.ok(!JSON.stringify(records).includes('secret-canary'));
    assert.match(readFileSync(path.join(config.outputParent, 'cargo-mutants.log'), 'utf8'), /secret-canary/);
  });
}

test('a long build produces recurring heartbeats even before an outcome exists', async t => {
  const config = fixture(t, 'heartbeat'), records = [];
  const code = await runMutations([], { ...config, cwd: config.root, pollMs: 5, heartbeatMs: 10,
    emit: record => {
      records.push(record);
      const builds = records.filter(row => row.stage === 'baseline' && row.phase === 'build');
      if (builds.length === 3) writeFileSync(path.join(config.outputParent, 'ack'), 'continue');
    },
  });
  assert.equal(code, 0);
  assert.ok(records.filter(row => row.stage === 'baseline' && row.phase === 'build').length >= 3);
  assert.equal(records.at(-1).completed, 3);
});

test('SIGTERM stops the owned mutation child and remains a failed cancellation', async t => {
  const config = fixture(t, 'signal');
  const child = spawn(process.execPath, [path.join(import.meta.dirname, 'mutation-progress.mjs'),
    config.outputParent, '--', '-p', 'rift-core'], {
    cwd: config.root, env: { ...process.env, PATH: `${config.root}${path.delimiter}${process.env.PATH}` },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  t.after(() => { if (child.exitCode === null && child.signalCode === null) child.kill('SIGTERM'); });
  let stdout = '', stderr = '', sent = false;
  child.stdout.on('data', chunk => {
    stdout += chunk;
    if (!sent && stdout.includes('"phase":"build"')) { sent = true; child.kill('SIGTERM'); }
  });
  child.stderr.on('data', chunk => { stderr += chunk; });
  const code = await new Promise((resolve, reject) => { child.once('error', reject); child.once('close', resolve); });
  assert.equal(code, 143, stderr);
  assert.ok(sent);
  assert.ok(!`${stdout}${stderr}`.includes('secret-canary'));
  assert.ok(stdout.includes('"exit_code":143'));
});

test('existing output is preserved instead of showing stale counters', async t => {
  const config = fixture(t);
  mkdirSync(path.join(config.outputParent, 'mutants.out'), { recursive: true });
  const sentinel = path.join(config.outputParent, 'mutants.out', 'sentinel');
  writeFileSync(sentinel, 'keep');
  await assert.rejects(runMutations([], { ...config, cwd: config.root }), /fresh/);
  assert.equal(readFileSync(sentinel, 'utf8'), 'keep');
  assert.ok(!existsSync(path.join(config.outputParent, 'cargo-mutants.log')));
});
