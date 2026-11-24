import test from 'node:test';
import assert from 'node:assert/strict';
import { visualSummary } from './visual-summary.mjs';

test('shows nested failing project and PNGs while escaping report-controlled HTML', () => {
  const report = { suites: [{ title: 'screens', suites: [{ title: '<suite>', specs: [{ title: 'visual connected', tests: [
    { projectName: 'desktop', status: 'unexpected', results: [{ attachments: [{ name: '<actual>', path: 'test-results/visual/a-actual.png' }, { name: 'trace', path: 'trace.zip' }] }] },
    { projectName: 'compact', status: 'expected', results: [] },
  ] }] }] }] };
  const summary = visualSummary(report, 'https://github.com/example/repo/actions/runs/1/artifacts/2', 'failure');
  assert.match(summary, /\[Download frontend-visual-diagnostics\]\(https:\/\/github.com/);
  assert.match(summary, /desktop \/ screens \/ &lt;suite&gt; \/ visual connected/);
  assert.match(summary, /&lt;actual&gt;/);
  assert.match(summary, /a-actual.png/);
  assert.doesNotMatch(summary, /trace.zip|compact|<suite>/);
  assert.match(summary, /failed check/);
});

test('setup failure without JSON or uploaded artifact gives log guidance', () => {
  const summary = visualSummary(undefined, '', 'failure');
  assert.match(summary, /artifact unavailable/);
  assert.match(summary, /inspect the visual step log/);
  assert.match(summary, /updates disabled/);
});

test('assertion failure without screenshot attachments is explained', () => {
  const summary = visualSummary({ suites: [{ specs: [{ title: 'visual', tests: [{ status: 'unexpected', results: [] }] }] }] }, '', 'failure');
  assert.match(summary, /no screenshot attachment/);
});
