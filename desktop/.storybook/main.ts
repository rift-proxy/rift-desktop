import type { StorybookConfig } from "@storybook/react-vite";
const config: StorybookConfig = {
  stories: ["../src/**/*.stories.tsx"],
  addons: [
    "@storybook/addon-docs",
    "@storybook/addon-a11y",
    "@storybook/addon-vitest",
  ],
  framework: "@storybook/react-vite",
  core: { disableTelemetry: true },
  viteFinal: (viteConfig) => ({ ...viteConfig, envDir: false }),
};
export default config;
