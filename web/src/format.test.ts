import { expect, test } from "bun:test";
import { diffStat, duration, result, tokens } from "./format";
import type { Tool } from "./protocol";

const tool = (patch: Partial<Tool>): Tool => ({
  kind: "tool", id: "1", name: "bash", title: "", state: "done", output: "", diff: null,
  lines: null, digest: null, live: "", progress: [], input: null, secs: null, ...patch,
});

test("formats numbers and times", () => {
  expect(tokens(950)).toBe("950");
  expect(tokens(12_400)).toBe("12k");
  expect(tokens(1_250_000)).toBe("1.3M");
  expect(duration(38)).toBe("38s");
  expect(duration(125)).toBe("2m 5s");
});

test("sums a diff, skipping file headers", () => {
  expect(diffStat("--- a/x\n+++ b/x\n@@\n-old\n+new\n+more\n")).toBe("+2 −1");
});

test("summarizes a call's result", () => {
  expect(result(tool({ output: "running 14 tests\n14 passed; 0 failed" }))).toBe("14 passed");
  expect(result(tool({ state: "failed", output: "Exit code 101\nerror[E0308]" }))).toBe("exit 101");
  expect(result(tool({ output: "a\nb" }))).toBe("2 lines");
  // A command that exited non-zero failed, even though the call itself worked.
  expect(result(tool({ output: "Exit code 1\nerror: test_add failed" }))).toBe("exit 1");
  expect(result(tool({ output: "(no output)" }))).toBe("ok");
  expect(result(tool({ name: "read", output: "1: fn main() {", lines: 631 }))).toBe("631 lines");
  expect(result(tool({ name: "grep", output: "x" }))).toBe("1 match");
  expect(result(tool({ name: "edit", diff: "-a\n+b\n" }))).toBe("+1 −1");
});
