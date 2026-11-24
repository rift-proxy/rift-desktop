// Render Playwright's JSON report as an Actions summary; never accept snapshots.
import { readFileSync, appendFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

const escape = (value) => String(value).replace(/[&<>]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' })[c]);

export function visualSummary(report, artifactUrl, outcome) {
  const lines = ['## Docker visual comparison', '', `Visual step: **${escape(outcome)}**. Baselines were compared with updates disabled.`, '',
    artifactUrl ? `[Download frontend-visual-diagnostics](${artifactUrl})` : 'Diagnostic artifact unavailable; inspect the upload step and visual log.', '',
    'Extract the artifact and open `playwright-report/index.html`. Screenshot attachments are also under `test-results/visual/`.', '',
    'A failed comparison stays a failed check. Review requirements and actual/diff images before proposing baseline changes in a reviewed PR.', ''];
  const failures = [];
  function visit(suite, parents = []) {
    const names = [...parents, suite.title].filter(Boolean);
    for (const spec of suite.specs ?? []) {
      for (const test of spec.tests ?? []) {
        if (test.status === 'expected' || test.status === 'skipped') continue;
        const attachments = (test.results ?? []).flatMap((r) => r.attachments ?? []).filter((a) => a.path?.endsWith('.png'));
        failures.push(`<li><code>${escape([test.projectName, ...names, spec.title].filter(Boolean).join(' / '))}</code> (${escape(test.status ?? 'unknown')})${attachments.length ? `<ul>${attachments.map((a) => `<li>${escape(a.name)}: <code>${escape(a.path)}</code></li>`).join('')}</ul>` : ' — no screenshot attachment (failure may precede the assertion).'}</li>`);
      }
    }
    for (const child of suite.suites ?? []) visit(child, names);
  }
  for (const suite of report?.suites ?? []) visit(suite);
  lines.push(failures.length ? `<details><summary>Failing screenshot/test details (${failures.length})</summary><ul>${failures.join('')}</ul></details>` : 'No failing test details found in JSON; for setup/reporting failures inspect the visual step log.');
  return lines.join('\n') + '\n';
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  let report;
  try { report = JSON.parse(readFileSync(process.argv[2], 'utf8')); } catch { /* Setup failure may not produce JSON. */ }
  appendFileSync(process.env.GITHUB_STEP_SUMMARY, visualSummary(report, process.env.VISUAL_ARTIFACT_URL, process.env.VISUAL_OUTCOME ?? 'unknown'));
}
