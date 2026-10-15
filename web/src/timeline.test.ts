import { expect, test } from "bun:test";
import { applyOps } from "./timeline";
import type { Item } from "./protocol";

test("streams text into the reply and output into a running tool", () => {
  let items: Item[] = [];
  items = applyOps(items, [{ op: "push", item: { kind: "assistant", text: "Hel", reasoning: "", done: false } }]);
  items = applyOps(items, [{ op: "append", index: 0, field: "text", text: "lo" }]);
  expect(items[0]).toEqual({ kind: "assistant", text: "Hello", reasoning: "", done: false });

  const tool: Item = {
    kind: "tool", id: "1", name: "bash", title: "ls", state: "running", output: "", diff: null,
    lines: null, digest: null, live: "", progress: [], input: null, secs: null,
  };
  items = applyOps(items, [{ op: "push", item: tool }, { op: "append", index: 1, field: "live", text: "a\n" }]);
  expect(items[1].kind === "tool" && items[1].live).toBe("a\n");
});

test("leaves the old array alone and truncates", () => {
  const before: Item[] = [{ kind: "user", text: "hi" }, { kind: "assistant", text: "x", reasoning: "", done: false }];
  const after = applyOps(before, [{ op: "truncate", len: 1 }]);
  expect(before.length).toBe(2);
  expect(after.length).toBe(1);
});
