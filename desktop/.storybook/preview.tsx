import type { Preview } from "@storybook/react-vite";
import "../src/theme.css";
const preview: Preview = {
  beforeEach: ({ parameters }) => {
    const previous = document.documentElement.dataset.theme;
    document.documentElement.dataset.theme = parameters.theme ?? "light";
    return () => {
      if (previous === undefined) delete document.documentElement.dataset.theme;
      else document.documentElement.dataset.theme = previous;
    };
  },
  parameters: {
    layout: "fullscreen",
    a11y: { test: "error" },
    viewport: {
      options: {
        desktop: {
          name: "1280 × 820",
          styles: { width: "1280px", height: "820px" },
        },
        compact: {
          name: "1000 × 680",
          styles: { width: "1000px", height: "680px" },
        },
      },
    },
  },
};
export default preview;
