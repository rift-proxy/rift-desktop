#!/usr/bin/env node
// Public CI evidence is deliberately aggregate-only: no source paths, errors, logs or traces.
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

function number(value) {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0)
    throw new Error("Invalid numeric frontend evidence");
  return value;
}

function count(value) {
  if (!Number.isSafeInteger(number(value)))
    throw new Error("Invalid frontend evidence count");
  return value;
}

export function summarizeFrontendEvidence(kind, report) {
  if (!report || typeof report !== "object" || Array.isArray(report))
    throw new Error("Invalid frontend evidence report");
  if (kind === "coverage") {
    const totals = {};
    for (const metric of ["lines", "statements", "functions", "branches"]) {
      const source = report.total?.[metric];
      if (!source) throw new Error("Missing frontend coverage metric");
      const metricTotal = {
        total: count(source.total),
        covered: count(source.covered),
        skipped: count(source.skipped),
        pct: number(source.pct),
      };
      const { total, covered, skipped, pct } = metricTotal;
      // Istanbul truncates to two decimal places and reports empty metrics as 100%.
      const expectedPct = total > 0 ? Math.floor(((100000 * covered) / total) / 10) / 100 : 100;
      if (covered > total || skipped > total || pct > 100 || pct !== expectedPct)
        throw new Error("Inconsistent frontend coverage metric");
      totals[metric] = metricTotal;
    }
    return { schema: 1, kind, totals };
  }
  if (kind === "mutation") {
    const counts = Object.fromEntries(
      ["Killed", "Survived", "NoCoverage", "Timeout", "CompileError", "RuntimeError", "Ignored", "Pending"].map((status) => [status, 0]),
    );
    if (!report.files || typeof report.files !== "object" || Array.isArray(report.files))
      throw new Error("Missing frontend mutation files");
    let total = 0;
    for (const file of Object.values(report.files)) {
      if (!Array.isArray(file?.mutants)) throw new Error("Missing frontend mutation results");
      for (const mutant of file.mutants) {
        if (!Object.hasOwn(counts, mutant?.status)) throw new Error("Unknown frontend mutation status");
        counts[mutant.status]++;
        total++;
      }
    }
    if (total === 0) throw new Error("Missing frontend mutants");
    return { schema: 1, kind, total, counts };
  }
  if (kind === "e2e" || kind === "visual") {
    const counts = Object.fromEntries(
      ["expected", "unexpected", "flaky", "skipped"].map((key) => [key, count(report.stats?.[key])]),
    );
    return { schema: 1, kind, counts, durationMs: number(report.stats?.duration) };
  }
  throw new Error("Unknown frontend evidence kind");
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [kind, input, output, ...extra] = process.argv.slice(2);
  if (!kind || !input || !output || extra.length)
    throw new Error("Usage: frontend-evidence.mjs KIND INPUT OUTPUT");
  const evidence = summarizeFrontendEvidence(kind, JSON.parse(readFileSync(input, "utf8")));
  mkdirSync(path.dirname(output), { recursive: true });
  writeFileSync(output, `${JSON.stringify(evidence, null, 2)}\n`);
}
