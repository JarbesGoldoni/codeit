import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { gitOf, store, useStore } from "../store";
import { ago } from "../format";
import { api, type Branch, type FileText, type Match, type Project } from "../code/api";
import { buildTree, parents, type TreeNode } from "../code/tree";
import { languageOf } from "../code/languages";
import { CodeView, type Selection } from "../code/CodeView";
import { FileIcon, FolderIcon, Icon } from "../code/Icon";
import { setColors, useSkin } from "../code/skin";
import { Review } from "./Review";

/** Browsing the project freely, or reviewing what the branch changed. */
type Mode = "files" | "review";

interface Tab {
  path: string;
  rev: string | null;
}

interface Find {
  word: string;
  x: number;
  y: number;
  rev: string | null;
  defs: Match[] | null;
  refs: Match[] | null;
  error?: string;
}

const short = (h: string) => h.slice(0, 7);

function local<T extends string>(key: string, fallback: T): T {
  try {
    return (localStorage.getItem(key) as T) || fallback;
  } catch {
    return fallback;
  }
}

function remember(key: string, v: string) {
  try {
    localStorage.setItem(key, v);
  } catch {
    // For this tab only.
  }
}

/** The text that points the agent at lines: `@path:12-18` (codeit attaches them), or, for an
 * older version, the path and the commit it is from. */
function reference(path: string, s: Selection | null, rev: string | null): string {
  const lines = s ? (s.from === s.to ? `:${s.from}` : `:${s.from}-${s.to}`) : "";
  if (rev) return `\`${path}${lines}\` (at ${short(rev)}) `;
  return `@${path}${lines} `;
}

export function Workbench() {
  const session = useStore((s) => s.session);
  const git = useStore(gitOf);
  const root = session?.project ?? "";
  const [mode, setModeState] = useState<Mode>(() =>
    local<string>("codeit.code.mode", "files") === "files" ? "files" : "review",
  );
  const setMode = (m: Mode) => {
    setModeState(m);
    remember("codeit.code.mode", m);
  };

  // ── files ──
  const [rev, setRev] = useState<string | null>(null);
  const [files, setFiles] = useState<string[]>([]);
  const [status, setStatus] = useState<Record<string, string>>({});
  const [treeError, setTreeError] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [tabs, setTabs] = useState<Tab[]>([]);
  const [active, setActive] = useState(0);
  const [content, setContent] = useState<FileText | null>(null);
  const [fileError, setFileError] = useState<string | null>(null);
  const [reveal, setReveal] = useState<{ line: number; at: number } | null>(null);
  const [filter, setFilter] = useState("");
  const [branches, setBranches] = useState<{ current: string | null; branches: Branch[] }>({ current: null, branches: [] });

  const [find, setFind] = useState<Find | null>(null);
  const workspace = useStore((s) => s.workspace);
  const tab = tabs[active] ?? null;

  // A new project: start over.
  useEffect(() => {
    setRev(null);
    setTabs([]);
    setActive(0);
    setExpanded(new Set());
    setFilter("");
  }, [root]);

  const loadTree = useCallback(() => {
    api
      .tree(root, rev)
      .then((t) => {
        setFiles(t.files);
        setStatus(t.status);
        setTreeError(null);
      })
      .catch((e) => setTreeError(String(e.message ?? e)));
  }, [root, rev]);

  const loadBranches = useCallback(() => {
    api.branches(root).then(setBranches).catch(() => setBranches({ current: null, branches: [] }));
  }, [root]);

  // The working tree changes as the agent works: look again when git says so.
  const gitKey = git ? `${git.branch}:${git.added}:${git.changed}` : "";
  useEffect(loadTree, [loadTree, gitKey]);
  useEffect(loadBranches, [loadBranches, gitKey]);

  // The open file.
  useEffect(() => {
    if (!tab) {
      setContent(null);
      return;
    }
    let live = true;
    api
      .file(root, tab.path, tab.rev)
      .then((f) => {
        if (!live) return;
        setContent(f);
        setFileError(null);
      })
      .catch((e) => live && setFileError(String(e.message ?? e)));
    return () => {
      live = false;
    };
  }, [root, tab?.path, tab?.rev, tab ? gitKey : ""]); // eslint-disable-line react-hooks/exhaustive-deps

  const openFile = (path: string, line: number | null, at: string | null = rev) => {
    setMode("files");
    if (at !== rev) setRev(at);
    setExpanded((e) => new Set([...e, ...parents(path)]));
    setTabs((ts) => {
      const i = ts.findIndex((t) => t.path === path && t.rev === at);
      if (i >= 0) {
        setActive(i);
        return ts;
      }
      setActive(ts.length);
      return [...ts, { path, rev: at }];
    });
    setReveal(line ? { line, at: Date.now() } : null);
    setFind(null);
  };

  const closeTab = (i: number) => {
    setTabs((ts) => ts.filter((_, j) => j !== i));
    setActive((a) => (a > i || a === tabs.length - 1 ? Math.max(0, a - 1) : a));
  };

  const ask = (text: string) => {
    store.toComposer(text, "insert");
    store.toast("Added to the prompt");
  };

  const lookUp = (word: string, x: number, y: number, jump: boolean, at: string | null, from: string | null) => {
    const f: Find = { word, x, y, rev: at, defs: null, refs: null };
    setFind(f);
    api
      .find(root, at, word)
      .then((r) => {
        // Nearest first: the same file, then the same folder, and so on.
        const near = (m: Match) => {
          if (!from) return 0;
          if (m.path === from) return -1000;
          const a = m.path.split("/");
          const b = from.split("/");
          let i = 0;
          while (i < a.length && i < b.length && a[i] === b[i]) i++;
          return -i;
        };
        r.defs.sort((a, b) => near(a) - near(b));
        if (jump && r.defs.length === 1) {
          openFile(r.defs[0].path, r.defs[0].line, at);
          return;
        }
        setFind((cur) => (cur && cur.word === word && cur.x === x ? { ...cur, defs: r.defs, refs: r.refs } : cur));
      })
      .catch((e) => setFind((cur) => (cur ? { ...cur, error: String(e.message ?? e) } : cur)));
  };

  return (
    <div className="workbench">
      <div className="wbbar">
        <div className="segmented">
          <button className={mode === "files" ? "on" : ""} onClick={() => setMode("files")}>
            <Icon id="files" /> Files
          </button>
          <button
            className={mode === "review" ? "on" : ""}
            onClick={() => setMode("review")}
            title="Review what this branch changed, file by file, and send your comments to the agent"
          >
            <Icon id="source-control" /> Code review
          </button>
        </div>
        <ProjectPicker />
        {mode === "files" && (
          <RevPicker
            branches={branches}
            value={rev}
            onChange={(r) => {
              setRev(r);
              setTabs((ts) => ts.map((t) => ({ ...t, rev: r })));
            }}
          />
        )}
        <span className="spacer" />
        <SkinToggle />
        <button className="iconbtn" title="Back to the conversation" onClick={() => store.setView("chat")}>
          <Icon id="close" />
        </button>
      </div>

      {workspace && !root ? (
        <div className="welcome">
          <Icon id="folder" size={40} />
          <p>codeit started in a folder of projects. Pick the one to work on: the agent moves there too.</p>
          <ProjectPicker compact />
        </div>
      ) : mode === "review" ? (
        <div className="wbbody">
          <Review
            root={root}
            gitKey={gitKey}
            onOpen={(path, line, at) => openFile(path, line, at)}
            onFind={(w, x, y, jump, at) => lookUp(w, x, y, jump, at, null)}
            onAsk={ask}
            reference={reference}
          />
        </div>
      ) : (
        <div className="wbbody">
          <aside className="wbside">
            <div className="wbsearch">
              <input
                placeholder="Go to file…"
                value={filter}
                onChange={(e) => setFilter(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Escape") setFilter("");
                }}
              />
              <button className="iconbtn small" title="Refresh" onClick={loadTree}>
                <Icon id="refresh" size={14} />
              </button>
              <button className="iconbtn small" title="Collapse folders" onClick={() => setExpanded(new Set())}>
                <Icon id="collapse-all" size={14} />
              </button>
            </div>
            {treeError && <div className="muted pad small">{treeError}</div>}
            {filter ? (
              <QuickOpen files={files} filter={filter} onOpen={(p) => openFile(p, null)} />
            ) : (
              <Explorer
                files={files}
                status={rev ? {} : status}
                expanded={expanded}
                setExpanded={setExpanded}
                selected={tab?.path ?? null}
                onOpen={(p) => openFile(p, null)}
              />
            )}
          </aside>

          <section className="wbmain">
            {tabs.length > 0 && (
              <div className="tabs">
                {tabs.map((t, i) => (
                  <div
                    key={`${t.rev}:${t.path}`}
                    className={`tab${i === active ? " on" : ""}`}
                    onClick={() => setActive(i)}
                    onAuxClick={(e) => e.button === 1 && closeTab(i)}
                    title={t.path}
                  >
                    <FileIcon path={t.path} />
                    <span>{t.path.split("/").pop()}</span>
                    {t.rev && <span className="tabrev">{short(t.rev)}</span>}
                    <button
                      className="tabx"
                      onClick={(e) => {
                        e.stopPropagation();
                        closeTab(i);
                      }}
                    >
                      ✕
                    </button>
                  </div>
                ))}
              </div>
            )}
            {tab && (
              <div className="crumbs">
                {tab.path.split("/").map((p, i, all) => (
                  <span key={i} className="crumb">
                    {i === all.length - 1 ? <FileIcon path={tab.path} /> : null}
                    {p}
                    {i < all.length - 1 && <Icon id="chevron-right" size={12} />}
                  </span>
                ))}
                <span className="spacer" />
                <button className="btn small" onClick={() => ask(reference(tab.path, null, tab.rev))}>
                  Reference the file
                </button>
              </div>
            )}
            {!tab && <Welcome files={files} onOpen={(p) => openFile(p, null)} />}
            {tab && fileError && <div className="muted pad">{fileError}</div>}
            {tab && content && content.path === tab.path && !fileError && (
              content.text === null ? (
                <div className="muted pad">
                  {content.too_big ? "This file is too large to show." : "Binary file."}
                </div>
              ) : (
                <CodeView
                  text={content.text}
                  lang={languageOf(tab.path)}
                  reveal={reveal}
                  onAsk={(s) => ask(reference(tab.path, s, tab.rev))}
                  onFind={(w, x, y, jump) => lookUp(w, x, y, jump, tab.rev, tab.path)}
                />
              )
            )}
          </section>
        </div>
      )}

      {find && <FindPopover find={find} onClose={() => setFind(null)} onOpen={(m) => openFile(m.path, m.line, find.rev)} />}
    </div>
  );
}

function Welcome({ files, onOpen }: { files: string[]; onOpen: (p: string) => void }) {
  const readme = files.find((f) => /^readme(\.md)?$/i.test(f));
  return (
    <div className="welcome">
      <Icon id="code" size={40} />
      <p>
        {files.length} files. Open one from the tree, or type its name in <b>Go to file</b>.
      </p>
      <p className="muted small">
        Click line numbers to pick lines (shift-click or drag for more) and reference them in the prompt. Click a
        name to see where it is defined and used; Ctrl- or ⌘-click jumps to its definition.
      </p>
      {readme && (
        <button className="btn" onClick={() => onOpen(readme)}>
          Open {readme}
        </button>
      )}
    </div>
  );
}

function Explorer({
  files,
  status,
  expanded,
  setExpanded,
  selected,
  onOpen,
}: {
  files: string[];
  status: Record<string, string>;
  expanded: Set<string>;
  setExpanded: (f: (e: Set<string>) => Set<string>) => void;
  selected: string | null;
  onOpen: (p: string) => void;
}) {
  const tree = useMemo(() => buildTree(files), [files]);
  // Folders holding changed files are marked too.
  const changedDirs = useMemo(() => {
    const m = new Map<string, string>();
    for (const [p, s] of Object.entries(status)) for (const d of parents(p)) if (!m.has(d)) m.set(d, s);
    return m;
  }, [status]);
  const sel = useRef<HTMLDivElement>(null);
  useEffect(() => {
    sel.current?.scrollIntoView({ block: "nearest" });
  }, [selected]);

  const render = (nodes: TreeNode[], depth: number): React.ReactNode =>
    nodes.map((n) => {
      if (n.dir) {
        const open = expanded.has(n.path);
        const s = changedDirs.get(n.path);
        return (
          <div key={n.path}>
            <div
              className={`trow${s ? ` g-${s}` : ""}`}
              style={{ paddingLeft: 8 + depth * 12 }}
              onClick={() =>
                setExpanded((e) => {
                  const x = new Set(e);
                  if (open) x.delete(n.path);
                  else x.add(n.path);
                  return x;
                })
              }
            >
              <span className={`chev${open ? " open" : ""}`}>
                <Icon id="chevron-right" size={12} />
              </span>
              <FolderIcon path={n.path} open={open} />
              <span className="tname">{n.name}</span>
              {s && <span className="gdot" />}
            </div>
            {open && <div className="tgroup">{render(n.children, depth + 1)}</div>}
          </div>
        );
      }
      const s = status[n.path];
      const on = n.path === selected;
      return (
        <div
          key={n.path}
          ref={on ? sel : undefined}
          className={`trow file${on ? " on" : ""}${s ? ` g-${s}` : ""}`}
          style={{ paddingLeft: 8 + depth * 12 + 14 }}
          onClick={() => onOpen(n.path)}
          title={n.path}
        >
          <FileIcon path={n.path} />
          <span className="tname">{n.name}</span>
          {s && <span className="gmark">{s === "added" ? "U" : s === "deleted" ? "D" : "M"}</span>}
        </div>
      );
    });

  return <div className="tree">{render(tree, 0)}</div>;
}

/** Files whose path holds the typed letters in order, best first. */
function QuickOpen({ files, filter, onOpen }: { files: string[]; filter: string; onOpen: (p: string) => void }) {
  const matches = useMemo(() => {
    const q = filter.toLowerCase().replace(/\s+/g, "");
    const scored: [number, string][] = [];
    for (const f of files) {
      const l = f.toLowerCase();
      const name = l.split("/").pop()!;
      let score: number;
      if (name.startsWith(q)) score = 0;
      else if (name.includes(q)) score = 1;
      else if (l.includes(q)) score = 2;
      else {
        let i = 0;
        for (const ch of l) if (ch === q[i]) i++;
        if (i < q.length) continue;
        score = 3;
      }
      scored.push([score * 1000 + f.length, f]);
    }
    return scored.sort((a, b) => a[0] - b[0]).slice(0, 80).map((s) => s[1]);
  }, [files, filter]);
  return (
    <div className="tree">
      {matches.map((f) => (
        <div key={f} className="trow file" style={{ paddingLeft: 10 }} onClick={() => onOpen(f)} title={f}>
          <FileIcon path={f} />
          <span className="tname">{f.split("/").pop()}</span>
          <span className="muted small ellipsis">{f.split("/").slice(0, -1).join("/")}</span>
        </div>
      ))}
      {!matches.length && <div className="muted pad small">No file matches.</div>}
    </div>
  );
}

function RevPicker({
  branches,
  value,
  onChange,
}: {
  branches: { current: string | null; branches: Branch[] };
  value: string | null;
  onChange: (rev: string | null) => void;
}) {
  if (!branches.branches.length && !branches.current) return null;
  const local = branches.branches.filter((b) => !b.remote);
  const remote = branches.branches.filter((b) => b.remote);
  const known = value === null || branches.branches.some((b) => b.name === value);
  return (
    <label className="revpicker" title="Show the files of">
      <Icon id="git-branch" size={14} />
      <select value={value ?? ""} onChange={(e) => onChange(e.target.value || null)}>
        <option value="">{`Working tree (${branches.current ?? "no branch"})`}</option>
        {!known && value && <option value={value}>{short(value)}</option>}
        <optgroup label="Branches">
          {local.map((b) => (
              <option key={b.name} value={b.name}>
                {b.name}
                {b.track ? ` [${b.track}]` : ""} · {ago(b.date)}
              </option>
            ))}
        </optgroup>
        {remote.length > 0 && (
          <optgroup label="Remote">
            {remote.map((b) => (
              <option key={b.name} value={b.name}>
                {b.name} · {ago(b.date)}
              </option>
            ))}
          </optgroup>
        )}
      </select>
    </label>
  );
}

/** The session's project: where the agent works, and what the viewer shows. */
export function ProjectPicker({ compact }: { compact?: boolean }) {
  const workspace = useStore((s) => s.workspace);
  const project = useStore((s) => s.session?.project ?? "");
  const cwd = useStore((s) => s.cwd);
  const git = useStore(gitOf);
  const [open, setOpen] = useState(false);
  const [list, setList] = useState<Project[] | null>(null);
  const [q, setQ] = useState("");
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    api.projects().then(setList).catch(() => setList([]));
    const close = (e: MouseEvent) => {
      if (!box.current?.contains(e.target as Node)) setOpen(false);
    };
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [open]);
  if (!workspace && !project) {
    // One project: nothing to pick, only the branch.
    return git ? (
      <span className="projchip static" title={cwd}>
        <Icon id="git-branch" size={14} /> {git.branch}
      </span>
    ) : null;
  }
  const name = project ? project.split("/").pop() : cwd.split("/").pop() || cwd;
  const shown = (list ?? []).filter((p) => !q || p.path.toLowerCase().includes(q.toLowerCase()));
  return (
    <div className="projpicker" ref={box}>
      <button
        className={`projchip${project ? "" : " none"}`}
        onClick={() => setOpen(!open)}
        title={project ? `${cwd}/${project}: the agent works here` : "Pick the project the agent works in"}
      >
        <Icon id="folder" size={14} />
        <span>{project ? name : compact ? "Pick a project" : `${name} (no project)`}</span>
        {git && project && (
          <span className="muted">
            <Icon id="git-branch" size={13} /> {git.branch}
            {git.added + git.changed > 0 && <span className="warn"> •{git.added + git.changed}</span>}
          </span>
        )}
        <Icon id="chevron-down" size={12} />
      </button>
      {open && (
        <div className="projmenu">
          <input autoFocus placeholder="Filter projects" value={q} onChange={(e) => setQ(e.target.value)} />
          {list === null && <div className="muted pad small">Looking…</div>}
          {shown.map((p) => (
            <button
              key={p.path}
              className={`projrow${p.path === project ? " on" : ""}`}
              onClick={() => {
                store.setProject(p.path);
                setOpen(false);
              }}
            >
              <Icon id={p.git ? "folder" : "files"} size={14} />
              <span className="pname">{p.path || `${p.name} (the whole folder)`}</span>
              {p.branch && (
                <span className="muted small">
                  <Icon id="git-branch" size={12} /> {p.branch}
                </span>
              )}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

function SkinToggle() {
  const skin = useSkin();
  if (!skin.hasColors) return null;
  return (
    <button
      className={`btn small${skin.colors ? " on" : ""}`}
      title={skin.colors ? `Using ${skin.name}'s colors: click for codeit's` : `Use ${skin.name}'s colors`}
      onClick={() => setColors(!skin.colors)}
    >
      {skin.name}
    </button>
  );
}

function FindPopover({ find, onClose, onOpen }: { find: Find; onClose: () => void; onOpen: (m: Match) => void }) {
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const close = (e: MouseEvent) => {
      if (!box.current?.contains(e.target as Node)) onClose();
    };
    const key = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    // After the click that opened it.
    const t = setTimeout(() => window.addEventListener("mousedown", close));
    window.addEventListener("keydown", key);
    return () => {
      clearTimeout(t);
      window.removeEventListener("mousedown", close);
      window.removeEventListener("keydown", key);
    };
  }, [onClose]);
  const left = Math.min(find.x, window.innerWidth - 480);
  const top = find.y + 14 + 360 > window.innerHeight ? Math.max(8, find.y - 374) : find.y + 14;
  const list = (title: string, ms: Match[]) => (
    <>
      <div className="fgroup">
        {title} <span className="muted">({ms.length})</span>
      </div>
      {ms.map((m, i) => (
        <button key={i} className="fmatch" onClick={() => onOpen(m)}>
          <span className="fwhere" title={m.path}>
            <FileIcon path={m.path} />
            {m.path.split("/").pop()}:{m.line}
            <span className="muted ellipsis">{m.path.split("/").slice(0, -1).join("/")}</span>
          </span>
          <code className="ftext">{m.text}</code>
        </button>
      ))}
    </>
  );
  return (
    <div className="findpop" ref={box} style={{ left, top }}>
      <div className="fhead">
        <code>{find.word}</code>
        {find.rev && <span className="muted small">at {short(find.rev)}</span>}
      </div>
      {find.error && <div className="muted pad small">{find.error}</div>}
      {!find.defs && !find.error && <div className="muted pad small">Looking…</div>}
      {find.defs && (
        <div className="flist">
          {find.defs.length > 0 ? list("Definitions", find.defs) : <div className="muted pad small">No definition found.</div>}
          {find.refs && find.refs.length > 0 && list("Uses", find.refs.slice(0, 60))}
        </div>
      )}
    </div>
  );
}
