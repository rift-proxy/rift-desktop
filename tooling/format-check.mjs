#!/usr/bin/env node
import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

export function formatCheckStatus(result) {
  if (result.error || result.signal || result.status === null) return { exitCode: 1, warning: false };
  if (result.status === 1) return { exitCode: 0, warning: true };
  return { exitCode: result.status, warning: false };
}

export function run() {
  const desktop = fileURLToPath(new URL('../desktop/', import.meta.url));
  const require = createRequire(resolve(desktop, 'package.json'));
  const packagePath = require.resolve('prettier/package.json');
  const manifest = JSON.parse(readFileSync(packagePath, 'utf8'));
  const binary = typeof manifest.bin === 'string' ? manifest.bin : manifest.bin?.prettier;
  if (!binary) throw new Error('The installed Prettier package has no CLI entrypoint');
  // Invoke Prettier itself: a pnpm wrapper's exit 1 could instead mean a missing
  // script/executable. Only Prettier's documented unformatted-files status is advisory.
  const result = spawnSync(process.execPath, [resolve(dirname(packagePath), binary), '--check', 'src', '*.ts', '.storybook'], {
    cwd: desktop,
    stdio: 'inherit',
  });
  if (result.error) console.error(result.error.message);
  if (result.signal) console.error(`Prettier terminated by ${result.signal}`);
  const { exitCode, warning } = formatCheckStatus(result);
  if (warning) {
    const message = 'Prettier found unformatted files; formatting is advisory. Run pnpm format to fix them.';
    console.warn(process.env.GITHUB_ACTIONS === 'true' ? `::warning::${message}` : `Warning: ${message}`);
  }
  return exitCode;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    process.exitCode = run();
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
