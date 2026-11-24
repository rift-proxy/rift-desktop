import test from 'node:test';
import assert from 'node:assert/strict';
import { formatCheckStatus } from './format-check.mjs';

test('Prettier formatting warnings are advisory, actual errors remain blocking', () => {
  assert.deepEqual(formatCheckStatus({ status: 0 }), { exitCode: 0, warning: false });
  assert.deepEqual(formatCheckStatus({ status: 1 }), { exitCode: 0, warning: true });
  assert.deepEqual(formatCheckStatus({ status: 2 }), { exitCode: 2, warning: false });
  assert.deepEqual(formatCheckStatus({ status: 127 }), { exitCode: 127, warning: false });
  assert.deepEqual(formatCheckStatus({ status: null, error: new Error('ENOENT') }), { exitCode: 1, warning: false });
  assert.deepEqual(formatCheckStatus({ status: null, signal: 'SIGTERM' }), { exitCode: 1, warning: false });
});
