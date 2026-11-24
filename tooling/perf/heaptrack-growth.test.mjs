import assert from "node:assert/strict";
import test from "node:test";
import { leakedBytes, leakGrowth } from "./heaptrack-growth.mjs";

test("sums exact leaked costs, including spaces in symbol names and CRLF", () => {
  assert.equal(leakedBytes("main;some function 1000\r\nmain;cache 24\r\n"), 1024n);
  assert.equal(leakedBytes(""), 0n);
  assert.equal(leakedBytes("main;cache 9007199254740993\n"), 9007199254740993n);
});

test("process-lifetime caches cancel and only growth above 4096 bytes fails", () => {
  assert.equal(leakGrowth("cache 1048576\n", "cache 1048576\nother 4096\n").regressed, false);
  const failure = leakGrowth("cache 1048576\n", "cache 1048576\nother 4097\n");
  assert.equal(failure.growth, 4097n);
  assert.equal(failure.regressed, true);
  assert.equal(leakGrowth("cache 2000\n", "cache 1000\n").regressed, false);
});

test("malformed or rounded costs are not converted into zero leaks", () => {
  for (const text of ["stack NaN\n", "stack -1\n", "stack 1.5\n", "stack 4KB\n", "123\n"])
    assert.throws(() => leakedBytes(text), /Malformed/);
});
