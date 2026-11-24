import { defineConfig } from "vitest/config";
import { yamlCatalog } from "./yamlCatalog";
export default defineConfig({
  plugins: [yamlCatalog()],
  test: { include: ["src/format.test.ts", "src/viewmodel.test.ts"] },
});
