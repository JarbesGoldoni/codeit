import type { Item, Op } from "./protocol";

// Keep in step with LIVE_MAX / LIVE_KEEP in timeline.rs.
const LIVE_MAX = 16_000;
const LIVE_KEEP = 8_000;

/** Applies the server's changes to a session's items (a new array; changed items are new objects). */
export function applyOps(items: Item[], ops: Op[]): Item[] {
  const out = items.slice();
  for (const op of ops) {
    switch (op.op) {
      case "push":
        out.push(op.item);
        break;
      case "set":
        out[op.index] = op.item;
        break;
      case "truncate":
        out.length = Math.min(out.length, op.len);
        break;
      case "append": {
        const it = out[op.index];
        if (!it) break;
        if (it.kind === "assistant" && (op.field === "text" || op.field === "reasoning")) {
          out[op.index] = { ...it, [op.field]: it[op.field] + op.text };
        } else if (it.kind === "tool" && op.field === "live") {
          let live = it.live + op.text;
          if (live.length > LIVE_MAX) live = live.slice(live.length - LIVE_KEEP);
          out[op.index] = { ...it, live };
        }
        break;
      }
    }
  }
  return out;
}
