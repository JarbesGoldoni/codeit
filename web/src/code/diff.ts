// Unified diffs (`git diff` output) read into files, hunks and lines.

export interface DiffLine {
  kind: "ctx" | "add" | "del";
  text: string;
  /** Line number in the old file (context and removed lines). */
  old: number | null;
  /** Line number in the new file (context and added lines). */
  new: number | null;
}

export interface Hunk {
  header: string;
  oldStart: number;
  newStart: number;
  lines: DiffLine[];
}

export interface DiffFile {
  /** The path after the change (before it, for a deleted file). */
  path: string;
  oldPath: string | null;
  status: "added" | "deleted" | "modified" | "renamed";
  binary: boolean;
  hunks: Hunk[];
  added: number;
  removed: number;
}

const unquote = (p: string) => (p.startsWith('"') && p.endsWith('"') ? p.slice(1, -1) : p);
const strip = (p: string) => unquote(p).replace(/^[ab]\//, "");

export function parseDiff(patch: string): DiffFile[] {
  const files: DiffFile[] = [];
  let f: DiffFile | null = null;
  let h: Hunk | null = null;
  let oldNo = 0;
  let newNo = 0;
  for (const line of patch.split("\n")) {
    if (line.startsWith("diff --git ")) {
      const m = /^diff --git (".*?"|\S+) (".*?"|\S+)$/.exec(line);
      const path = m ? strip(m[2]) : line.slice(11);
      f = { path, oldPath: null, status: "modified", binary: false, hunks: [], added: 0, removed: 0 };
      h = null;
      files.push(f);
      continue;
    }
    if (!f) continue;
    if (!h || !/^[ +\-\\]/.test(line)) {
      if (line.startsWith("new file mode")) f.status = "added";
      else if (line.startsWith("deleted file mode")) f.status = "deleted";
      else if (line.startsWith("rename from ")) {
        f.oldPath = line.slice(12);
        f.status = "renamed";
      } else if (line.startsWith("rename to ")) f.path = line.slice(10);
      else if (line.startsWith("Binary files ")) f.binary = true;
      else if (line.startsWith("--- ")) {
        const p = line.slice(4);
        if (p !== "/dev/null" && f.status === "deleted") f.path = strip(p);
      } else if (line.startsWith("@@")) {
        const m = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@(.*)$/.exec(line);
        if (!m) continue;
        oldNo = Number(m[1]);
        newNo = Number(m[2]);
        h = { header: m[3].trim(), oldStart: oldNo, newStart: newNo, lines: [] };
        f.hunks.push(h);
      }
      continue;
    }
    const c = line[0];
    const text = line.slice(1);
    if (c === "+") {
      h.lines.push({ kind: "add", text, old: null, new: newNo++ });
      f.added++;
    } else if (c === "-") {
      h.lines.push({ kind: "del", text, old: oldNo++, new: null });
      f.removed++;
    } else if (c === " ") {
      h.lines.push({ kind: "ctx", text, old: oldNo++, new: newNo++ });
    }
    // "\ No newline at end of file" is left out.
  }
  return files;
}
