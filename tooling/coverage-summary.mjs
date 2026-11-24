#!/usr/bin/env node
// Publish coverage numbers, never LLVM's source-bearing files/functions/paths.
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export function coverageTotals(report) {
  const totals = report?.data?.[0]?.totals;
  if (!Array.isArray(report?.data) || report.data.length !== 1 || !totals)
    throw new Error("Expected one complete LLVM coverage totals record");
  const result = { schema: 1, metrics: {} };
  for (const name of ["lines", "functions", "regions", "branches", "instantiations", "mcdc"]) {
    if (totals[name] === undefined) continue;
    const metric = {};
    for (const key of ["count", "covered", "notcovered", "percent"]) {
      const value = totals[name][key];
      if (value === undefined) continue;
      if (typeof value !== "number" || !Number.isFinite(value) || value < 0 ||
          (key === "percent" ? value > 100 : !Number.isSafeInteger(value)))
        throw new Error("Invalid numeric LLVM coverage total");
      metric[key] = value;
    }
    if (metric.count === undefined || metric.covered === undefined || metric.percent === undefined)
      throw new Error("Incomplete LLVM coverage total");
    result.metrics[name] = metric;
  }
  if (!["lines", "functions", "regions"].every((name) => result.metrics[name]))
    throw new Error("Missing required LLVM coverage totals");
  return result;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const [input, output, ...extra] = process.argv.slice(2);
    if (!input || !output || extra.length) throw new Error("Expected input and output");
    const summary = coverageTotals(JSON.parse(readFileSync(input, "utf8")));
    mkdirSync(path.dirname(output), { recursive: true });
    writeFileSync(output, `${JSON.stringify(summary, null, 2)}\n`);
    console.log("Numeric Rust coverage summary prepared.");
  } catch { console.error("Rust coverage summary failed; raw report withheld."); process.exitCode = 1; }
}
