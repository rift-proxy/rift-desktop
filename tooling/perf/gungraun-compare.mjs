#!/usr/bin/env node
// Compare independently measured Gungraun 0.19.4 summary-v6 instruction counts.
// A missing baseline/case is a configuration error, never an initial PASS.
import { readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export function instructionCounts(directory) {
  const counts = new Map();
  for (const entry of readdirSync(directory, { recursive: true, withFileTypes: true })) {
    if (!entry.isFile() || entry.name !== "summary.json") continue;
    const file = path.join(entry.parentPath, entry.name);
    const summary = JSON.parse(readFileSync(file, "utf8"));
    if (summary.version !== "6" || typeof summary.module_path !== "string" || !summary.module_path ||
        !(summary.id === null || typeof summary.id === "string")) {
      throw new Error("Unsupported or malformed Gungraun summary");
    }
    const profiles = summary.profiles?.filter((profile) => profile.tool === "Callgrind");
    if (profiles?.length !== 1) throw new Error("Expected exactly one Callgrind profile per benchmark");
    const metric = profiles[0].summaries?.total?.summary?.Callgrind?.Ir?.metrics;
    // Each side is a fresh measurement in its own home, not an implicit old cache baseline.
    const instructions = metric?.Left?.Int;
    if (!Number.isSafeInteger(instructions) || instructions <= 0) {
      throw new Error("Missing positive fresh Callgrind instruction count");
    }
    const key = JSON.stringify([summary.module_path, summary.id]);
    if (counts.has(key)) throw new Error("Duplicate Gungraun benchmark identity");
    counts.set(key, instructions);
  }
  if (counts.size === 0) throw new Error("No Gungraun summaries: compatible baseline and current measurements are required");
  return counts;
}

export function compareInstructions(baseline, current) {
  if (baseline.size === 0 || current.size === 0 || baseline.size !== current.size ||
      [...current.keys()].some((key) => !baseline.has(key))) {
    throw new Error("Incompatible Gungraun benchmark inventories; no missing baseline is permitted");
  }
  return [...current].sort(([a], [b]) => a.localeCompare(b)).map(([benchmark, instructions]) => {
    const previous = baseline.get(benchmark);
    if (![previous, instructions].every((value) => Number.isSafeInteger(value) && value > 0)) {
      throw new Error("Invalid Gungraun instruction count");
    }
    return {
      benchmark,
      baseline: previous,
      current: instructions,
      changePercent: (instructions / previous - 1) * 100,
      regressed: BigInt(instructions) * 100n > BigInt(previous) * 110n,
    };
  });
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  try {
    if (process.argv.length !== 4) throw new Error("usage: gungraun-compare.mjs BASELINE_HOME CURRENT_HOME");
    const rows = compareInstructions(instructionCounts(process.argv[2]), instructionCounts(process.argv[3]));
    console.log(JSON.stringify({ instructionRegressionLimitPercent: 10, benchmarks: rows }, null, 2));
    if (rows.some((row) => row.regressed)) process.exitCode = 1;
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
