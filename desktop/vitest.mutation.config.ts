import { defineConfig } from "vitest/config";
import { yamlCatalog } from "./yamlCatalog";
export default defineConfig({
  envDir: false,
  plugins: [yamlCatalog()],
  test: { include: ["src/format.test.ts", "src/viewmodel.test.ts"] },
});
