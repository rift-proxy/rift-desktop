#!/usr/bin/env node
// Rift-owned tooling: size budgets for shipped artefacts (docs/build.md).
//   node tooling/perf/size-budget.mjs [--require a,b] [--report-only a,b] [--update]
// Budgets live in tooling/perf/budgets.json: measured bytes + allowed growth. An artefact
// that is not built is skipped unless listed in --require. Growth over the tolerance fails;
// --report-only prints it without failing (sizes measured on another toolchain);
// --update writes the measured sizes as the new baseline (review the diff).
import { readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { CORE_SPEC } from "../sidecar.mjs";

const root = path.resolve(import.meta.dirname, "../..");
const budgetsFile = path.join(root, "tooling/perf/budgets.json");

export function validateComponentEvidence(budgets, base = root) {
  for (const [name, artifact] of Object.entries(budgets.artifacts)) {
    for (const allowance of artifact.component_allowances ?? []) {
      const relative = allowance.evidence;
      if (
        typeof relative !== "string" ||
        path.isAbsolute(relative) ||
        relative.split(/[\\/]/).includes("..")
      )
        throw new Error("Invalid component evidence path");
      const proof = JSON.parse(readFileSync(path.join(base, relative), "utf8"));
      if (
        name !== "installer" ||
        proof.schema !== 1 ||
        proof.xray?.sha256 !== CORE_SPEC.xray.windows ||
        proof.xray?.version !== CORE_SPEC.xray.version ||
        proof.difference_bytes !== allowance.bytes ||
        proof.full_installer?.bytes - proof.size_control_without_xray?.bytes !==
          allowance.bytes ||
        proof.original_base_budget_bytes !== artifact.bytes ||
        proof.original_tolerance !== artifact.tolerance ||
        proof.control_not_shipped !== true ||
        proof.size_control_without_xray.bytes >
          Math.round(artifact.bytes * (1 + artifact.tolerance))
      )
        throw new Error(
          "Component allowance does not match measured Xray ablation or original base budget",
        );
    }
  }
}

/** Files matching `pattern`: a path whose last segment may contain `*`. */
export function expand(pattern, base = root) {
  const dir = path.join(base, path.dirname(pattern));
  const name = path.basename(pattern);
  if (!name.includes("*")) {
    try {
      return statSync(path.join(dir, name)).isFile()
        ? [path.join(dir, name)]
        : [];
    } catch {
      return [];
    }
  }
  const re = new RegExp(`^${name.split("*").map(escape).join(".*")}$`);
  try {
    return readdirSync(dir)
      .filter((f) => re.test(f))
      .map((f) => path.join(dir, f))
      .filter((f) => statSync(f).isFile());
  } catch {
    return [];
  }
}
const escape = (s) => s.replace(/[.+?^${}()|[\]\\]/g, "\\$&");

/** Bytes of an artefact: the first candidate that exists; sum or largest of its files. */
export function measure(artifact, base = root) {
  for (const candidate of artifact.paths) {
    const sizes = expand(candidate, base).map((f) => statSync(f).size);
    if (sizes.length) {
      return artifact.largest
        ? Math.max(...sizes)
        : sizes.reduce((a, b) => a + b, 0);
    }
  }
  return null;
}

/** One row per artefact: status ok | grew | shrank | missing | new. */
export function compare(
  budgets,
  measured,
  { require = [], reportOnly = [] } = {},
) {
  return Object.entries(budgets.artifacts).map(([name, artifact]) => {
    const bytes = measured[name];
    const tolerance = artifact.tolerance ?? budgets.tolerance;
    if (bytes === null || bytes === undefined) {
      return { name, status: "missing", fail: require.includes(name) };
    }
    if (!artifact.bytes) return { name, bytes, status: "new", fail: false };
    const allowances = artifact.component_allowances ?? [];
    if (
      !Array.isArray(allowances) ||
      allowances.length > 1 ||
      allowances.some(
        (entry) =>
          name !== "installer" ||
          entry.name !== "xray" ||
          entry.version !== CORE_SPEC.xray.version ||
          entry.binary_sha256 !== CORE_SPEC.xray.windows ||
          !Number.isSafeInteger(entry.bytes) ||
          entry.bytes <= 0,
      )
    )
      throw new Error("Invalid or wrong-core component allowance");
    const added = allowances.reduce((sum, entry) => sum + entry.bytes, 0);
    const baseline = artifact.bytes + added;
    const change = bytes / baseline - 1;
    // Whole-byte limits: 1020 bytes against 1000 at 2% is on budget, not over by rounding.
    const grew = bytes > Math.round(baseline * (1 + tolerance));
    const shrank = bytes < Math.round(baseline * (1 - tolerance));
    return {
      name,
      bytes,
      baseline,
      baseBaseline: artifact.bytes,
      componentAllowance: added,
      change,
      tolerance,
      status: grew ? "grew" : shrank ? "shrank" : "ok",
      fail: grew && !reportOnly.includes(name),
    };
  });
}

export function format(rows) {
  const kib = (n) => `${(n / 1024).toFixed(1)} KiB`;
  return rows
    .map((r) => {
      if (r.status === "missing")
        return `${r.fail ? "FAIL" : "skip"}  ${r.name}: not built`;
      if (r.status === "new")
        return `new   ${r.name}: ${kib(r.bytes)} (no baseline, run --update)`;
      const pct = `${r.change >= 0 ? "+" : ""}${(100 * r.change).toFixed(2)}%`;
      const label = r.fail
        ? "FAIL"
        : r.status === "grew"
          ? "warn"
          : r.status === "shrank"
            ? "down"
            : "ok";
      const hint =
        r.status === "grew"
          ? ` > +${(100 * r.tolerance).toFixed(0)}% budget`
          : r.status === "shrank"
            ? " (smaller)"
            : "";
      const components = r.componentAllowance
        ? ` (original base ${kib(r.baseBaseline)} + measured Xray ${kib(r.componentAllowance)})`
        : "";
      return `${label.padEnd(5)} ${r.name}: ${kib(r.bytes)} vs ${kib(r.baseline)} (${pct})${hint}${components}`;
    })
    .join("\n");
}

function parseArgs(args, artifacts) {
  const options = { require: [], reportOnly: [], update: false };
  const seen = new Set();
  for (let i = 0; i < args.length; i++) {
    const flag = args[i];
    if (!["--require", "--report-only", "--update"].includes(flag) || seen.has(flag))
      throw new Error(`Unknown or repeated size-budget argument: ${flag}`);
    seen.add(flag);
    if (flag === "--update") {
      options.update = true;
      continue;
    }
    const value = args[++i];
    const names = value?.split(",") ?? [];
    if (!names.length || names.some((name) => !Object.hasOwn(artifacts, name)))
      throw new Error(`Invalid artifact list for ${flag}`);
    options[flag === "--require" ? "require" : "reportOnly"] = names;
  }
  return options;
}

function main(args) {
  const budgets = JSON.parse(readFileSync(budgetsFile, "utf8"));
  const options = parseArgs(args, budgets.artifacts);
  const measured = Object.fromEntries(
    Object.entries(budgets.artifacts).map(([name, a]) => [name, measure(a)]),
  );
  validateComponentEvidence(budgets);
  if (options.update) {
    for (const [name, bytes] of Object.entries(measured)) {
      if (bytes !== null) {
        if (budgets.artifacts[name].component_allowances?.length)
          throw new Error(
            "Update the base and measured component cost separately; combined baseline must not overwrite the original",
          );
        budgets.artifacts[name].bytes = bytes;
      }
    }
    writeFileSync(budgetsFile, `${JSON.stringify(budgets, null, 2)}\n`);
  }
  const rows = compare(budgets, measured, options);
  console.log(format(rows));
  return rows.some((r) => r.fail) ? 1 : 0;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  process.exitCode = main(process.argv.slice(2));
}
