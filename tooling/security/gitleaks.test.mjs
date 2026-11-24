import assert from 'node:assert/strict';
import test from 'node:test';
import { resultSummary } from './gitleaks.mjs';

test('only aggregate findings survive the Gitleaks boundary', () => {
  const sensitive = 'do-not-disclose-sensitive-fixture';
  const result = resultSummary(1, [{ Secret: sensitive, Match: sensitive, File: sensitive, Commit: sensitive, Message: sensitive }]);
  assert.deepEqual(result, { findings: 1, passed: false });
  assert.equal(JSON.stringify(result).includes(sensitive), false);
});
test('a completed clean scan passes', () => {
  assert.deepEqual(resultSummary(0, []), { findings: 0, passed: true });
});
test('configuration failures and malformed or contradictory reports fail closed', () => {
  for (const [status, report] of [[null, []], [127, []], [2, []], [0, {}], [0, [{}]], [1, []]]) {
    assert.throws(() => resultSummary(status, report));
  }
});
