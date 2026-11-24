#!/usr/bin/env node
// heaptrack_print's leaked-cost folded stacks retain exact bytes, unlike rounded prose reports.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

export function leakedBytes(foldedStacks) {
  let bytes = 0n;
  for (const line of foldedStacks.split(/\r?\n/)) {
    if (!line.trim()) continue;
    const match = /\S[^\r\n]*\s+(\d+)\s*$/.exec(line);
    if (!match) throw new Error("Malformed Heaptrack leaked-cost folded stack");
    bytes += BigInt(match[1]);
  }
  return bytes;
}

export function leakGrowth(baseline, repeated) {
  const oneRoundBytes = leakedBytes(baseline);
  const fiftyRoundBytes = leakedBytes(repeated);
  const growth = fiftyRoundBytes - oneRoundBytes;
  return { oneRoundBytes, fiftyRoundBytes, growth, regressed: growth > 4096n };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  try {
    if (process.argv.length !== 4) throw new Error("usage: heaptrack-growth.mjs ONE_ROUND_FOLDED FIFTY_ROUNDS_FOLDED");
    const result = leakGrowth(readFileSync(process.argv[2], "utf8"), readFileSync(process.argv[3], "utf8"));
    console.log(`Leak growth over 1 to 50 rounds: ${result.growth} B (${result.oneRoundBytes} B -> ${result.fiftyRoundBytes} B); limit 4096 B`);
    if (result.regressed) process.exitCode = 1;
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
