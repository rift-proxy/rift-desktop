import { defineConfig } from "@playwright/test";
const storybookUrl = `http://127.0.0.1:${process.env.STORYBOOK_PORT ?? "6006"}`;
export default defineConfig({
  testDir: "./e2e",
  fullyParallel: true,
  workers: process.env.CI ? 2 : "50%",
  timeout: 30000,
  expect: { timeout: 7000, toHaveScreenshot: { maxDiffPixelRatio: 0.002 } },
  use: {
    baseURL: storybookUrl,
    timezoneId: "Europe/Moscow",
    locale: "ru-RU",
    colorScheme: "light",
    trace: "retain-on-failure",
  },
  projects: [
    { name: "desktop", use: { viewport: { width: 1280, height: 820 } } },
    { name: "compact", use: { viewport: { width: 1000, height: 680 } } },
  ],
  webServer: {
    command: "node serve-storybook.mjs",
    url: storybookUrl,
    // Never attach to another checkout's stale Storybook build on the same port.
    reuseExistingServer: false,
  },
  reporter: [["list"], ["html", { open: "never" }]],
});
