#!/usr/bin/env node
// Public CI retains numeric mutation outcomes, never child logs, source diffs or command details.
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export function mutationSummary(outcomes) {
  if (outcomes?.cargo_mutants_version !== "27.1.0")
    throw new Error("Unsupported mutation outcome schema");
  const result = { schema: 1, tool: "cargo-mutants", version: "27.1.0" };
  for (const key of ["total_mutants", "caught", "missed", "timeout", "unviable", "success"]) {
    if (!Number.isSafeInteger(outcomes[key]) || outcomes[key] < 0)
      throw new Error("Invalid mutation outcome counters");
    result[key] = outcomes[key];
  }
  return result;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  try {
    if (process.argv.length !== 4) throw new Error("Expected mutation outcome input and aggregate output paths");
    const result = mutationSummary(JSON.parse(readFileSync(process.argv[2], "utf8")));
    mkdirSync(path.dirname(process.argv[3]), { recursive: true });
    writeFileSync(process.argv[3], `${JSON.stringify(result, null, 2)}\n`);
  } catch {
    console.error("Mutation aggregate unavailable or invalid");
    process.exitCode = 1;
  }
}
