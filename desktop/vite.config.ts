/// <reference types="node" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { yamlCatalog } from "./yamlCatalog";

export default defineConfig({
  // Build inputs come from the caller's process, never neighboring dotenv files.
  envDir: false,
  plugins: [react(), yamlCatalog()],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: {
    target: "es2022",
    sourcemap: false,
  },
});
