#!/usr/bin/env node
// Keep Cargo output private; publish only phases, counters and elapsed time.
import { spawn } from 'node:child_process';
import { closeSync, existsSync, mkdirSync, openSync, readFileSync, readSync, statSync } from 'node:fs';
import { constants as osConstants } from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { mutationSummary } from './mutation-summary.mjs';

const phases = new Set(['Check', 'Build', 'Test']);
const counters = ['caught', 'missed', 'timeout', 'unviable', 'success'];
const signalCodes = { SIGINT: 130, SIGTERM: 143 };

/** Project the pinned tool's persisted state; no command, path or mutation text escapes. */
export function progressSnapshot(outcomes, total, debug, elapsedSeconds) {
  if (!Number.isSafeInteger(total) || total < 0) throw new Error('Invalid mutation plan');
  const summary = mutationSummary(outcomes);
  if (!Array.isArray(outcomes.outcomes) || summary.total_mutants > total)
    throw new Error('Invalid mutation progress');
  const tally = Object.fromEntries(counters.map(key => [key, 0]));
  const categories = { CaughtMutant: 'caught', MissedMutant: 'missed', Timeout: 'timeout',
    Unviable: 'unviable', Success: 'success' };
  let baseline;
  let completed = 0;
  for (const outcome of outcomes.outcomes) {
    if (outcome?.scenario === 'Baseline') {
      if (baseline || completed || !['Success', 'Failure', 'Timeout'].includes(outcome.summary))
        throw new Error('Invalid baseline outcome');
      baseline = outcome;
    } else {
      const mutant = outcome?.scenario?.Mutant;
      if (baseline?.summary !== 'Success' || !mutant || typeof mutant !== 'object' || Array.isArray(mutant) ||
        !Object.hasOwn(categories, outcome.summary)) throw new Error('Invalid mutant outcome');
      tally[categories[outcome.summary]]++;
      completed++;
    }
  }
  if (completed !== summary.total_mutants || counters.some(key => tally[key] !== summary[key]))
    throw new Error('Inconsistent mutation outcome counters');
  let phase = null;
  for (const match of debug.matchAll(/\brun\{phase=(Check|Build|Test)\}:[^\n]*?\b(start process|process_status=)/g))
    phase = match[2] === 'start process' && phases.has(match[1]) ? match[1].toLowerCase() : null;
  const stage = baseline?.summary === 'Success' ? 'mutants'
    : baseline ? 'baseline-failed' : 'baseline';
  return { event: 'mutation-progress', stage, phase, completed: summary.total_mutants, total,
    ...Object.fromEntries(counters.map(key => [key, summary[key]])), elapsed_seconds: elapsedSeconds };
}

function tail(file) {
  const descriptor = openSync(file, 'r');
  try {
    const size = statSync(file).size;
    const bytes = Buffer.alloc(Math.min(size, 65536));
    readSync(descriptor, bytes, 0, bytes.length, size - bytes.length);
    return bytes.toString('utf8');
  } finally { closeSync(descriptor); }
}

const emptyOutcome = () => ({ cargo_mutants_version: '27.1.0', outcomes: [], total_mutants: 0,
  caught: 0, missed: 0, timeout: 0, unviable: 0, success: 0 });

export async function runMutations(args, { outputParent, cwd = process.cwd(), heartbeatMs = 15000,
  pollMs = 1000, emit = record => console.log(JSON.stringify(record)), command = 'cargo' } = {}) {
  if (!outputParent || args.some(arg => arg === '-o' || arg.startsWith('--output') || /^-o./.test(arg)))
    throw new Error('Expected an output directory and mutation arguments without output overrides');
  const output = path.resolve(outputParent);
  const directory = path.join(output, 'mutants.out');
  if (existsSync(directory)) throw new Error('Mutation output must be fresh');
  mkdirSync(output, { recursive: true });
  const log = openSync(path.join(output, 'cargo-mutants.log'), 'wx', 0o600);
  const started = performance.now();
  let last = null;
  let lastKey = '';
  let lastPrinted = -Infinity;
  let valid = false;
  let baselinePassed = false;
  const sample = (force = false) => {
    const elapsed = Math.floor((performance.now() - started) / 1000);
    let current = last ? { ...last, elapsed_seconds: elapsed }
      : { event: 'mutation-progress', stage: 'preparing', phase: null, completed: 0, total: null,
        caught: 0, missed: 0, timeout: 0, unviable: 0, success: 0, elapsed_seconds: elapsed };
    valid = false;
    try {
      const plan = JSON.parse(readFileSync(path.join(directory, 'mutants.json'), 'utf8'));
      if (!Array.isArray(plan)) throw new Error('Invalid mutation plan');
      let outcomes = emptyOutcome();
      if (existsSync(path.join(directory, 'outcomes.json'))) {
        outcomes = JSON.parse(readFileSync(path.join(directory, 'outcomes.json'), 'utf8'));
        valid = true;
      }
      current = progressSnapshot(outcomes, plan.length, tail(path.join(directory, 'debug.log')), elapsed);
      baselinePassed = outcomes.outcomes.some(outcome => outcome?.scenario === 'Baseline' && outcome.summary === 'Success');
    } catch {
      // The native tool writes JSON in place: a partial snapshot is transient while it runs.
      valid = false;
    }
    const { elapsed_seconds, ...changing } = current;
    const key = JSON.stringify(changing);
    const now = performance.now();
    if (force || key !== lastKey || now - lastPrinted >= heartbeatMs) {
      emit(current);
      lastKey = key;
      lastPrinted = now;
    }
    last = current;
  };
  sample(true);
  const child = spawn(command, ['mutants', '--output', output, '--colors', 'never', '--annotations', 'none', ...args], {
    cwd, stdio: ['ignore', log, log], detached: process.platform !== 'win32',
  });
  closeSync(log);
  let interrupted = null;
  const handlers = new Map(Object.keys(signalCodes).map(signal => [signal, () => {
    interrupted = signal;
    if (!child.pid) return;
    try {
      if (process.platform === 'win32') child.kill(signal);
      else process.kill(-child.pid, signal);
    } catch (error) { if (error.code !== 'ESRCH') throw error; }
  }]));
  for (const [signal, handler] of handlers) process.on(signal, handler);
  const timer = setInterval(sample, pollMs);
  let result;
  try {
    result = await new Promise(resolve => {
      child.once('error', () => resolve({ code: 1, signal: null }));
      child.once('close', (code, signal) => resolve({ code, signal }));
    });
  } finally {
    clearInterval(timer);
    for (const [signal, handler] of handlers) process.off(signal, handler);
  }
  sample(true);
  const nativeSignal = osConstants.signals[result.signal];
  let code = interrupted ? signalCodes[interrupted]
    : result.code ?? (Number.isSafeInteger(nativeSignal) ? 128 + nativeSignal : 1);
  if (code === 0 && (!valid || !baselinePassed || last.total < 1 || last.completed !== last.total || last.missed || last.timeout)) code = 1;
  emit({ ...last, stage: code === 0 ? 'finished' : 'failed', exit_code: code });
  return code;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    if (process.argv[3] !== '--' || process.argv.length < 5) throw new Error('Invalid arguments');
    process.exitCode = await runMutations(process.argv.slice(4), { outputParent: process.argv[2] });
  } catch {
    console.error('Mutation progress runner failed: invalid arguments, output or process setup');
    process.exitCode = 1;
  }
}
