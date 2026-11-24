// Rift-owned tooling: visual regression tests in the official Playwright image, so the
// baselines do not depend on the fonts of the machine that runs them (local or CI).
// Usage: node tooling/visual-docker.mjs [extra playwright args, e.g. --update-snapshots]
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

const root = resolve(import.meta.dirname, '..');
const version = JSON.parse(
  readFileSync(resolve(root, 'desktop/node_modules/@playwright/test/package.json'), 'utf8'),
).version;
const image = `mcr.microsoft.com/playwright:v${version}-noble`;
const user = process.getuid ? [`--user`, `${process.getuid()}:${process.getgid()}`] : [];
execFileSync(
  'docker',
  [
    'run', '--rm', '--ipc=host', ...user,
    '--env', 'HOME=/tmp', '--env', 'CI',
    ...(process.env.PLAYWRIGHT_JSON_OUTPUT_FILE ? ['--env', 'PLAYWRIGHT_JSON_OUTPUT_FILE'] : []),
    ...(process.env.PLAYWRIGHT_HTML_OUTPUT_DIR ? ['--env', 'PLAYWRIGHT_HTML_OUTPUT_DIR'] : []),
    ...(process.env.PLAYWRIGHT_HTML_OPEN ? ['--env', 'PLAYWRIGHT_HTML_OPEN'] : []),
    '--volume', `${root}:/work`,
    '--workdir', '/work/desktop',
    image,
    'npx', 'playwright', 'test', '--grep', 'visual', ...process.argv.slice(2),
  ],
  { stdio: 'inherit' },
);
