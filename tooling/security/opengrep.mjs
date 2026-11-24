import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

const nonemptyString = value => typeof value === 'string' && value.length > 0;
const position = value => Number.isSafeInteger(value?.line) && value.line > 0 &&
  Number.isSafeInteger(value.col) && value.col > 0 &&
  Number.isSafeInteger(value.offset) && value.offset >= 0;

/** Findings stay visible; only ERROR findings and broken analysis block the build. */
export function assess(report) {
  if (!nonemptyString(report?.version) ||
      !Array.isArray(report.results) || !Array.isArray(report.errors) ||
      !Array.isArray(report.paths?.scanned) || !report.paths.scanned.every(nonemptyString))
    throw new Error('Invalid OpenGrep report envelope');
  const findings = { ERROR: 0, WARNING: 0, INFO: 0 };
  const diagnostics = { error: 0, warn: 0, info: 0 };
  for (const result of report.results) {
    if (!nonemptyString(result?.check_id) || !nonemptyString(result.path) ||
        !position(result.start) || !position(result.end) ||
        result.end.offset < result.start.offset || result.end.line < result.start.line ||
        (result.end.line === result.start.line && result.end.col < result.start.col) ||
        typeof result.extra?.message !== 'string')
      throw new Error('Invalid OpenGrep finding location or metadata');
    const severity = result?.extra?.severity;
    if (typeof severity !== 'string' || !Object.hasOwn(findings, severity))
      throw new Error('Invalid OpenGrep finding severity');
    findings[severity]++;
  }
  for (const diagnostic of report.errors) {
    const kind = Array.isArray(diagnostic?.type) ? diagnostic.type[0] : diagnostic?.type;
    if (!Number.isSafeInteger(diagnostic?.code) || diagnostic.code < 0 ||
        !nonemptyString(kind) || typeof diagnostic.message !== 'string')
      throw new Error('Invalid OpenGrep diagnostic metadata');
    const level = diagnostic?.level;
    if (typeof level !== 'string' || !Object.hasOwn(diagnostics, level))
      throw new Error('Invalid OpenGrep diagnostic level');
    diagnostics[level]++;
  }
  return { findings, diagnostics, failed: findings.ERROR > 0 || diagnostics.error > 0 };
}

function safePath(value) {
  if (typeof value !== 'string') return undefined;
  const relative = value.replaceAll('\\', '/');
  if (relative.startsWith('/') || /^[A-Za-z]:/.test(relative) ||
      relative.includes('://') || relative.split('/').includes('..'))
    return '[redacted path]';
  return relative;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    if (process.argv.length !== 3) throw new Error('Usage: node tooling/security/opengrep.mjs <report.json>');
    const report = JSON.parse(readFileSync(process.argv[2], 'utf8'));
    const { findings, diagnostics, failed } = assess(report);
    for (const result of report.results) {
      console.log(JSON.stringify({
        severity: result.extra.severity,
        rule: typeof result.check_id === 'string' ? result.check_id : undefined,
        path: safePath(result.path),
        line: Number.isSafeInteger(result.start?.line) ? result.start.line : undefined,
      }));
    }
    for (const diagnostic of report.errors) {
      const kind = Array.isArray(diagnostic.type) ? diagnostic.type[0] : diagnostic.type;
      console.log(JSON.stringify({
        analysis: diagnostic.level,
        kind: typeof kind === 'string' ? kind : undefined,
        path: safePath(diagnostic.path),
      }));
    }
    console.log(`OpenGrep findings: ERROR=${findings.ERROR}, WARNING=${findings.WARNING}, INFO=${findings.INFO}`);
    console.log(`OpenGrep analysis: error=${diagnostics.error}, warn=${diagnostics.warn}, info=${diagnostics.info}`);
    if (diagnostics.warn) console.warn('OpenGrep analysis has warnings; some files or rules were not fully analyzed.');
    process.exitCode = failed ? 1 : 0;
  } catch (error) {
    console.error('OpenGrep gate failed: missing, malformed or unsupported report, or invalid arguments.');
    process.exitCode = 1;
  }
}
