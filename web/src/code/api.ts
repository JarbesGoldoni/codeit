// The code viewer's requests (crates/codeit/src/web/code.rs).

export interface Project {
  /** Relative to where codeit started ("" for it). */
  path: string;
  name: string;
  branch: string | null;
  git: boolean;
}

export interface Branch {
  name: string;
  remote: boolean;
  hash: string;
  date: number;
  subject: string;
  upstream: string | null;
  track: string;
}

export interface Commit {
  hash: string;
  parents: string[];
  author: string;
  email: string;
  date: number;
  refs: string[];
  subject: string;
  body: string;
  reviewed: boolean;
}

export interface FileText {
  path: string;
  text: string | null;
  binary?: boolean;
  too_big?: boolean;
  size?: number;
}

export interface Match {
  path: string;
  line: number;
  text: string;
}

export interface DiffResult {
  patch: string;
  cut: boolean;
  base: string;
  commit: { hash: string; author: string; date: number; subject: string; body: string } | null;
}

async function call<T>(body: object): Promise<T> {
  const r = await fetch("/api/code", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  const v = await r.json().catch(() => ({ error: `codeit answered ${r.status}` }));
  if (!r.ok) throw new Error(v.error ?? `codeit answered ${r.status}`);
  return v as T;
}

export const api = {
  projects: () => call<{ projects: Project[] }>({ op: "projects" }).then((v) => v.projects),
  tree: (root: string, rev: string | null) =>
    call<{ files: string[]; status: Record<string, "added" | "modified" | "deleted"> }>({ op: "tree", root, rev }),
  file: (root: string, path: string, rev: string | null) => call<FileText>({ op: "file", root, path, rev }),
  branches: (root: string) => call<{ current: string | null; branches: Branch[] }>({ op: "branches", root }),
  log: (root: string, rev: string | null, skip = 0) =>
    call<{ commits: Commit[]; more: boolean; uncommitted: number }>({ op: "log", root, rev, skip }),
  diff: (root: string, from: string | null, to: string | null) => call<DiffResult>({ op: "diff", root, from, to }),
  reviewed: (root: string, hashes: string[], on: boolean) => call<{ ok: boolean }>({ op: "reviewed", root, hashes, on }),
  find: (root: string, rev: string | null, name: string) =>
    call<{ defs: Match[]; refs: Match[] }>({ op: "find", root, rev, name }),
};
