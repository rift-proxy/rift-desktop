import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import { yamlCatalog } from "./yamlCatalog";
import { storybookTest } from "@storybook/addon-vitest/vitest-plugin";
import { playwright } from "@vitest/browser-playwright";
import fs from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";

/**
 * pnpm's virtual store (`node_modules/.pnpm`) that actually serves this project's dependencies,
 * found from where a real dependency resolves: next to `package.json` for a standalone checkout,
 * in a parent workspace's root when this package is a nested member. No fixed relative path.
 */
export function pnpmStoreDir(
  resolveFrom: string = import.meta.url,
): string | undefined {
  const manifest = fs.realpathSync(
    createRequire(resolveFrom).resolve("vitest/package.json"),
  );
  const marker = `${path.sep}node_modules${path.sep}.pnpm${path.sep}`;
  const at = manifest.indexOf(marker);
  return at < 0 ? undefined : manifest.slice(0, at + marker.length - 1);
}

const pnpmStore = pnpmStoreDir();
export default defineConfig({
  envDir: false,
  test: {
    coverage: {
      provider: "v8",
      reportOnFailure: true,
      include: [
        "src/format.ts",
        "src/viewmodel.ts",
        "src/backend/provider.tsx",
        "src/backend/metrics.ts",
        "src/components/**/*.tsx",
        "src/App.tsx",
      ],
      exclude: ["**/*.stories.tsx", "**/*.test.tsx", "src/testing/**"],
    },
    projects: [
      {
        envDir: false,
        plugins: [react(), yamlCatalog()],
        test: {
          name: "unit",
          environment: "jsdom",
          include: ["src/**/*.test.{ts,tsx}"],
          setupFiles: ["src/testing/setup.ts"],
          // Multi-step userEvent flows (type, click, waitFor) hit the 5000ms default under a
          // loaded CI runner even though the same run is fast locally; seen flaking in CI.
          testTimeout: 10000,
        },
      },
      {
        envDir: false,
        plugins: [
          storybookTest({
            configDir: path.resolve(import.meta.dirname, ".storybook"),
          }),
          // The stories import the texts (`locales/ru.yml`) like the page does.
          yamlCatalog(),
        ],
        // desktop has its own pnpm-workspace.yaml (standalone installs), which makes Vite
        // narrow its auto-detected fs.allow to desktop itself. When this package is a
        // member of a parent pnpm workspace, the hoisted node_modules/.pnpm/* packages this
        // browser project needs at runtime then live outside that directory and Vite answers
        // 403. Allow exactly the pnpm store the dependencies resolve from (not a whole parent
        // directory); in a standalone checkout it is already inside desktop.
        server: {
          fs: {
            allow: [
              path.resolve(import.meta.dirname),
              ...(pnpmStore ? [pnpmStore] : []),
            ],
          },
        },
        test: {
          name: "storybook",
          browser: {
            enabled: true,
            headless: true,
            // Parallel Chromium renderers can deadlock before test collection on the
            // release runner. Keep browser stories deterministic; unit tests remain parallel.
            fileParallelism: false,
            provider: playwright(),
            instances: [{ browser: "chromium" }],
          },
        },
      },
    ],
  },
});
