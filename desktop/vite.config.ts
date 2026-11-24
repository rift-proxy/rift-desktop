/// <reference types="node" />
import { mkdirSync, readdirSync, renameSync } from "node:fs";
import { join, resolve } from "node:path";
import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import { yamlCatalog } from "./yamlCatalog";

/**
 * With `RIFT_MAPS_DIR` set, the build makes source maps and moves them out of the
 * embedded `dist`. CI retains matching JS/maps as a separate public artifact for
 * diagnosing that exact build; maps are not included in the installer.
 */
function mapsBesideTheBuild(directory: string): Plugin {
  let outDir = "";
  return {
    name: "rift-maps-beside-the-build",
    apply: "build",
    configResolved(config) {
      outDir = resolve(config.root, config.build.outDir);
    },
    closeBundle() {
      const assets = join(outDir, "assets");
      const kept = join(resolve(directory), "assets");
      mkdirSync(kept, { recursive: true });
      for (const name of readdirSync(assets).filter((n) => n.endsWith(".map")))
        renameSync(join(assets, name), join(kept, name));
    },
  };
}

const mapsDirectory = process.env.RIFT_MAPS_DIR;

export default defineConfig({
  plugins: [
    react(),
    yamlCatalog(),
    ...(mapsDirectory ? [mapsBesideTheBuild(mapsDirectory)] : []),
  ],
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: {
    target: "es2022",
    // "hidden": the map exists, the file does not point at it.
    sourcemap: mapsDirectory ? "hidden" : false,
  },
});
