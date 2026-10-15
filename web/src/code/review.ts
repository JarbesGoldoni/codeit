// A code review, as on GitHub but local: files marked viewed, comments on lines, all sent to the
// agent at once. Kept in the browser, per project and branch, until sent.

import type { DiffFile } from "./diff";

export interface Comment {
  id: string;
  path: string;
  /** The lines (in the file after the change), or null for the whole file. */
  from: number | null;
  to: number | null;
  /** The commit the lines are from, or null for the working tree. */
  rev: string | null;
  /** The lines commented on, to quote. */
  quote: string;
  text: string;
}

/** Viewed files: path → the fingerprint of its changes when marked. Changed since, it is
 * shown again. */
export type Viewed = Record<string, string>;

/** Changes the same way, same fingerprint. */
export function fingerprint(f: DiffFile): string {
  let h = 0x811c9dc5;
  const add = (s: string) => {
    for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 0x01000193);
  };
  add(`${f.status}:${f.oldPath ?? ""}:${f.binary}`);
  for (const hk of f.hunks) for (const l of hk.lines) add(`${l.kind}${l.text}\n`);
  return (h >>> 0).toString(36);
}

export type ViewState = "viewed" | "changed" | "new";

export function viewState(viewed: Viewed, f: DiffFile): ViewState {
  const fp = viewed[f.path];
  if (!fp) return "new";
  return fp === fingerprint(f) ? "viewed" : "changed";
}

export function load<T>(key: string, fallback: T): T {
  try {
    const v = localStorage.getItem(key);
    return v ? (JSON.parse(v) as T) : fallback;
  } catch {
    return fallback;
  }
}

export function save(key: string, v: unknown) {
  try {
    localStorage.setItem(key, JSON.stringify(v));
  } catch {
    // For this tab only.
  }
}

const where = (c: Comment) => {
  const lines = c.from === null ? "" : c.from === c.to ? `:${c.from}` : `:${c.from}-${c.to}`;
  return `${c.path}${lines}${c.rev ? ` (at ${c.rev.slice(0, 7)})` : ""}`;
};

/** The message the agent gets: every comment under the lines it is about, then the summary.
 * Plain text, as it shows in the conversation. */
export function reviewText(comments: Comment[], summary: string, scope: string): string {
  const out = [`My code review of ${scope}. Address each comment; ask if one is unclear.`];
  const sorted = [...comments].sort((a, b) => a.path.localeCompare(b.path) || (a.from ?? 0) - (b.from ?? 0));
  for (const c of sorted) {
    out.push("", `${where(c)}${c.from === null ? " (the whole file)" : ""}:`);
    if (c.quote) {
      const lines = c.quote.split("\n");
      const shown = lines.length > 8 ? [...lines.slice(0, 8), "…"] : lines;
      out.push(...shown.map((l) => `> ${l}`));
    }
    out.push(c.text.trim());
  }
  if (summary.trim()) out.push("", `Overall: ${summary.trim()}`);
  return out.join("\n");
}

/** The lines `from`–`to` of the file after the change, as far as the diff shows them. */
export function quoteOf(f: DiffFile, from: number, to: number): string {
  const out: string[] = [];
  for (const h of f.hunks) for (const l of h.lines) if (l.new !== null && l.new >= from && l.new <= to) out.push(l.text);
  return out.join("\n");
}
