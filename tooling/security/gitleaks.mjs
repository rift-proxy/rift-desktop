import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

// Never return filenames, matches, secrets, commit messages, or scanner output.
export function resultSummary(status, report) {
  if (status !== 0 && status !== 1) throw new Error('Gitleaks could not complete; check installation and configuration.');
  if (!Array.isArray(report)) throw new Error('Gitleaks did not produce a JSON findings array.');
  if ((status === 0) !== (report.length === 0)) throw new Error('Gitleaks status/report mismatch.');
  return { findings: report.length, passed: status === 0 };
}

export function scan(mode) {
  if (!['worktree', 'history'].includes(mode)) throw new Error('Expected worktree or history.');
  if (mode === 'history') {
    const shallow = spawnSync('git', ['rev-parse', '--is-shallow-repository'], { encoding: 'utf8' });
    if (shallow.status !== 0 || shallow.stdout.trim() !== 'false') throw new Error('History scanning requires a full clone.');
  }
  const dir = mkdtempSync(join(tmpdir(), 'rift-gitleaks-'));
  try {
    const reportPath = join(dir, 'redacted.json');
    const args = [mode === 'history' ? 'git' : 'dir', '--config', 'gitleaks-public.toml', '--redact=100', '--no-banner', '--report-format', 'json', '--report-path', reportPath, '--exit-code', '1'];
    if (mode === 'history') args.push('--log-opts=--all');
    args.push('.');
    // Even redacted scanner diagnostics can contain sensitive source lines. Discard
    // both streams; only the aggregate below is allowed into logs/artifacts.
    const run = spawnSync('gitleaks', args, { stdio: 'ignore' });
    if (run.error || ![0, 1].includes(run.status)) throw new Error('Gitleaks could not complete; check installation and configuration.');
    let report;
    try { report = JSON.parse(readFileSync(reportPath, 'utf8')); }
    catch { throw new Error('Gitleaks did not produce a readable JSON report.'); }
    const summary = resultSummary(run.status, report);
    console.log(`Gitleaks ${mode}: ${summary.findings} findings; ${summary.passed ? 'PASS' : 'FAIL'}. Finding details are not retained.`);
    return summary.passed ? 0 : 1;
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try { process.exitCode = scan(process.argv[2]); }
  catch (error) { console.error(error.message); process.exitCode = 1; }
}
