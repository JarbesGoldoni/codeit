import type { Tool } from "./protocol";

export function tokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1000) return `${Math.round(n / 1000)}k`;
  return String(n);
}

export function duration(secs: number): string {
  if (secs < 60) return `${Math.round(secs)}s`;
  const m = Math.floor(secs / 60);
  const s = Math.round(secs % 60);
  return s ? `${m}m ${s}s` : `${m}m`;
}

export function ago(unixSecs: number): string {
  const d = Date.now() / 1000 - unixSecs;
  if (d < 60) return "just now";
  if (d < 3600) return `${Math.floor(d / 60)}m ago`;
  if (d < 86400) return `${Math.floor(d / 3600)}h ago`;
  if (d < 86400 * 7) return `${Math.floor(d / 86400)}d ago`;
  return new Date(unixSecs * 1000).toLocaleDateString();
}

/** The verb a tool call shows as. */
export function verb(name: string): string {
  const verbs: Record<string, string> = {
    read: "Read",
    write: "Write",
    edit: "Edit",
    multiedit: "Edit",
    apply_patch: "Patch",
    bash: "Run",
    grep: "Search",
    glob: "Find",
    list: "List",
    ls: "List",
    webfetch: "Fetch",
    fetch: "Fetch",
    websearch: "Search web",
    task: "Agent",
    todowrite: "Todos",
    todoread: "Todos",
    question: "Ask",
    skill: "Skill",
    lsp: "Inspect",
    review_comment: "Comment",
  };
  if (verbs[name]) return verbs[name];
  // MCP tools: server_tool.
  return name.replace(/_/g, " ").replace(/^\w/, (c) => c.toUpperCase());
}

/** `+8 −3` for a diff. */
export function diffStat(diff: string): string {
  let add = 0;
  let del = 0;
  for (const l of diff.split("\n")) {
    if (l.startsWith("+++") || l.startsWith("---")) continue;
    if (l.startsWith("+")) add++;
    else if (l.startsWith("-")) del++;
  }
  return `+${add} −${del}`;
}

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

/** The exit code bash reported (its output starts with `Exit code N`). */
export function bashExit(output: string): number | null {
  const m = /^Exit code (-?\d+)/.exec(output);
  return m ? Number(m[1]) : null;
}

function outputLines(t: Tool): number {
  const body = t.output.trim();
  if (!body || body === "(no output)") return 0;
  return t.lines ?? body.split("\n").length - (bashExit(body) !== null ? 1 : 0);
}

/** A call that went wrong: it failed, or it is a command that exited non-zero. */
export function failed(t: Tool): boolean {
  return t.state === "failed" || (t.name === "bash" && t.state === "done" && bashExit(t.output) !== null);
}

/** What a call came to, on the right of its row (as the terminal app shows it). */
export function result(t: Tool): string {
  if (t.state === "pending") return "";
  if (t.state === "running") return t.progress.at(-1) ?? "";
  if (failed(t)) {
    const code = bashExit(t.output);
    if (code !== null) return `exit ${code}`;
    return t.output === "Interrupted." ? "interrupted" : "failed";
  }
  switch (t.name) {
    case "edit":
    case "write":
    case "apply_patch":
      if (t.diff) return diffStat(t.diff);
      return t.output.startsWith("Created") ? "new file" : "";
    case "read":
      return plural(outputLines(t), "line");
    case "grep":
      return plural(outputLines(t), "match", "matches");
    case "glob":
      return plural(outputLines(t), "file");
    case "bash": {
      const passed = /(\d+) passed/.exec(t.output);
      if (passed) return `${passed[1]} passed`;
      const n = outputLines(t);
      return n ? plural(n, "line") : "ok";
    }
    case "task":
      return "done";
    default:
      return "";
  }
}
