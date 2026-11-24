#!/usr/bin/env node
// Rift-owned tooling: text summary of a Samply profile for the public CPU profiling check.
//   node tooling/cpu-profile-summary.mjs <profile.json.gz> [--top N]
// Needs the `<profile>.syms.json` sidecar (`samply record --unstable-presymbolicate`).
// Sections: time per perf_workload scenario, per nearest Rift function (callees such as
// malloc/memcpy/clone included), and the hottest functions by self time.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { gunzipSync } from "node:zlib";

const RIFT = /^(<)?(rift_core|perf_workload)::/;

/** Readable function name: no hash suffix, no generic arguments, no closure suffix. */
export function simplify(name) {
  let out = name.replace(/::h[0-9a-f]{16}$/, "");
  for (let previous; previous !== out;) {
    previous = out;
    out = out.replace(/([\w>])<[^<>]*>/g, "$1");
  }
  return out
    .replace(/^<([^<>\s]+)>::/, "$1::")
    .replace(/(::\{closure#\d+\})+/g, "")
    .replace(/(::\{async_fn_body#\d+\})+/g, "");
}

/** Symbol lookup by library debug name and relative address; innermost inline frame first
 * where the sidecar has inline frames for that exact address. */
function symbolizer(syms) {
  const libraries = new Map();
  for (const lib of syms.data) {
    const table = [...lib.symbol_table].sort((a, b) => a.rva - b.rva);
    libraries.set(lib.debug_name, table);
  }
  return (library, address) => {
    const table = libraries.get(library);
    if (!table) return [library];
    let lo = 0;
    let hi = table.length - 1;
    let found = -1;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      if (table[mid].rva <= address) {
        found = mid;
        lo = mid + 1;
      } else hi = mid - 1;
    }
    const entry = table[found];
    if (!entry || address >= entry.rva + entry.size)
      return [`${library}+0x${address.toString(16)}`];
    // `frames` is the inline stack at the symbol's start address only; applying it to
    // every address of the function would put all of `main` inside its first inlined call.
    const frames =
      entry.frames?.length && address === entry.rva
        ? entry.frames
        : [{ function: entry.symbol }];
    return frames.map((f) => syms.string_table[f.function]);
  };
}

export function summarize(profile, syms) {
  const lookup = symbolizer(syms);
  const self = new Map();
  const nearest = new Map();
  const scenarios = new Map();
  const add = (map, key, weight) => map.set(key, (map.get(key) ?? 0) + weight);
  let total = 0;
  let first = Infinity;
  let last = -Infinity;
  // Scenarios run one after another; spawned tasks (the coordinator worker) are polled by
  // the runtime, not under the scenario's frame, so they are attributed by time window.
  const windows = new Map();
  const unowned = [];
  for (const thread of profile.threads) {
    const { frameTable, funcTable, resourceTable, stackTable, samples } =
      thread;
    const cache = new Map();
    const namesOf = (frame) => {
      if (!cache.has(frame)) {
        const func = frameTable.func[frame];
        const library =
          profile.libs[resourceTable.lib[funcTable.resource[func]]];
        // Frames outside any library (kernel, JIT) keep the name samply gave them.
        const names = library
          ? lookup(library.debugName, frameTable.address[frame])
          : [thread.stringArray[funcTable.name[func]] ?? "[unknown]"];
        cache.set(frame, names.map(simplify));
      }
      return cache.get(frame);
    };
    for (let i = 0; i < samples.length; i++) {
      const stack = samples.stack[i];
      if (stack === null) continue;
      const weight = samples.weight?.[i] ?? 1;
      const time = samples.time[i];
      total += weight;
      first = Math.min(first, time);
      last = Math.max(last, time);
      let owner = null;
      let scenario = null;
      for (
        let s = stack, top = true;
        s !== null;
        s = stackTable.prefix[s], top = false
      ) {
        const names = namesOf(stackTable.frame[s]);
        if (top) add(self, names[0], weight);
        for (const name of names) {
          if (!owner && RIFT.test(name) && name !== "perf_workload::main")
            owner = name;
          const found = /^perf_workload::(\w+)$/.exec(name)?.[1];
          if (found && found !== "main") scenario = found;
        }
      }
      add(nearest, owner ?? "(outside Rift code: startup, runtime)", weight);
      if (scenario) {
        add(scenarios, scenario, weight);
        const w = windows.get(scenario) ?? [time, time];
        windows.set(scenario, [Math.min(w[0], time), Math.max(w[1], time)]);
      } else unowned.push([time, weight]);
    }
  }
  if (!Number.isFinite(total) || total <= 0)
    throw new Error("Samply captured no CPU samples; check hosted perf permissions and recording configuration");
  for (const [time, weight] of unowned) {
    const scenario = [...windows].find(
      ([, [from, to]]) => time >= from && time <= to,
    )?.[0];
    add(scenarios, scenario ?? "(setup and shutdown)", weight);
  }
  const ranked = (map) => [...map].sort((a, b) => b[1] - a[1]);
  return {
    total,
    seconds: total ? (last - first) / 1000 : 0,
    scenarios: ranked(scenarios),
    nearest: ranked(nearest),
    self: ranked(self),
  };
}

export function format(summary, top = 12) {
  const pct = (n) =>
    `${((100 * n) / (summary.total || 1)).toFixed(1).padStart(5)}%`;
  const rows = (list) =>
    list.slice(0, top).map(([name, n]) => `  ${pct(n)}  ${name.slice(0, 120)}`);
  return [
    `Samples: ${summary.total}, wall ${summary.seconds.toFixed(2)} s`,
    "",
    "Scenarios (perf_workload, inclusive; spawned tasks by time):",
    ...rows(summary.scenarios),
    "",
    "Nearest Rift function (its malloc/memcpy/clone included):",
    ...rows(summary.nearest),
    "",
    "Self time (where the CPU actually is):",
    ...rows(summary.self),
  ].join("\n");
}

function main(args) {
  const file = args.find((a) => !a.startsWith("--"));
  const topAt = args.indexOf("--top");
  const top = topAt >= 0 ? Number(args[topAt + 1]) : 12;
  if (!file) {
    console.error("usage: cpu-profile-summary.mjs <profile.json.gz> [--top N]");
    return 2;
  }
  const profile = JSON.parse(gunzipSync(readFileSync(file)));
  const symsFile = file.replace(/\.json(\.gz)?$/, ".json.syms.json");
  let syms;
  try {
    syms = JSON.parse(readFileSync(symsFile, "utf8"));
  } catch {
    console.error(
      `${symsFile} not found: record with samply --unstable-presymbolicate`,
    );
    return 1;
  }
  console.log(format(summarize(profile, syms), top));
  return 0;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  process.exitCode = main(process.argv.slice(2));
}
