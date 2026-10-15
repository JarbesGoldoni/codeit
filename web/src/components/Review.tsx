import { useEffect, useMemo, useRef, useState } from "react";
import { modelOf, store } from "../store";
import { ago } from "../format";
import { api, type Commit, type ReviewInfo } from "../code/api";
import { parseDiff, type DiffFile } from "../code/diff";
import { DiffView, type ReviewHooks } from "../code/DiffView";
import { FileIcon, Icon } from "../code/Icon";
import type { Selection } from "../code/CodeView";
import { fingerprint, load, reviewText, save, viewState, type Comment, type Viewed } from "../code/review";

/** git's empty tree: the base of a range that starts at the first commit. */
const EMPTY_TREE = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/** What the diff shows: everything under review, or a part of it, or an older commit. */
type Scope =
  | { kind: "all" }
  | { kind: "working" }
  | { kind: "commits"; hashes: string[] }
  | { kind: "history"; commit: Commit };

interface Changes {
  files: DiffFile[];
  cut: boolean;
}

const short = (h: string) => h.slice(0, 7);

/** A code review of the current branch, as on GitHub: what it adds over the branch it came from
 * (and what isn't committed yet), file by file. Files are marked viewed, lines get comments, and
 * the comments go to the agent at once. */
export function Review({
  root,
  gitKey,
  onOpen,
  onFind,
  onAsk,
  reference,
}: {
  root: string;
  gitKey: string;
  onOpen: (path: string, line: number | null, rev: string | null) => void;
  onFind: (word: string, x: number, y: number, jump: boolean, rev: string | null) => void;
  onAsk: (text: string) => void;
  reference: (path: string, s: Selection | null, rev: string | null) => string;
}) {
  const [info, setInfo] = useState<ReviewInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [all, setAll] = useState<Changes | null>(null);
  const [scope, setScope] = useState<Scope>({ kind: "all" });
  const [part, setPart] = useState<Changes | null>(null);
  const [loading, setLoading] = useState(false);
  const [anchor, setAnchor] = useState<string | null>(null);
  const [history, setHistory] = useState<{ commits: Commit[]; more: boolean } | null>(null);
  const [scrollTo, setScrollTo] = useState<string | null>(null);
  const [tick, setTick] = useState(0);

  // What you marked and wrote, per project and branch, kept until sent.
  const key = info ? `${root}:${info.branch ?? "HEAD"}` : null;
  const [viewed, setViewed] = useState<Viewed>({});
  const [comments, setComments] = useState<Comment[]>([]);
  useEffect(() => {
    if (!key) return;
    setViewed(load(`codeit.review.viewed:${key}`, {}));
    setComments(load(`codeit.review.comments:${key}`, []));
  }, [key]);
  const keep = (v: Viewed, c: Comment[]) => {
    if (!key) return;
    save(`codeit.review.viewed:${key}`, v);
    save(`codeit.review.comments:${key}`, c);
  };
  const updateViewed = (f: (v: Viewed) => Viewed) =>
    setViewed((v) => {
      const n = f(v);
      keep(n, comments);
      return n;
    });
  const updateComments = (f: (c: Comment[]) => Comment[]) =>
    setComments((c) => {
      const n = f(c);
      keep(viewed, n);
      return n;
    });

  // A new project or branch: from the top.
  useEffect(() => {
    setScope({ kind: "all" });
    setHistory(null);
  }, [root, info?.branch]);

  useEffect(() => {
    let live = true;
    api
      .review(root)
      .then((r) => {
        if (!live) return;
        setInfo(r);
        setError(null);
      })
      .catch((e) => live && setError(String(e.message ?? e)));
    return () => {
      live = false;
    };
  }, [root, gitKey, tick]);

  // Everything under review: from where the branch starts to the files as they are now.
  useEffect(() => {
    if (!info) return;
    let live = true;
    setLoading(true);
    api
      .diff(root, info.base, null)
      .then((d) => live && setAll({ files: parseDiff(d.patch), cut: d.cut }))
      .catch((e) => live && setError(String(e.message ?? e)))
      .finally(() => live && setLoading(false));
    return () => {
      live = false;
    };
  }, [root, info, tick]);

  // A part of it, or an older commit.
  const scopeKey =
    scope.kind === "commits" ? scope.hashes.join(",") : scope.kind === "history" ? scope.commit.hash : scope.kind;
  useEffect(() => {
    setPart(null);
    if (scope.kind === "all" || !info) return;
    let from: string | null = null;
    let to: string | null = null;
    if (scope.kind === "commits") {
      const chosen = info.commits.filter((c) => scope.hashes.includes(c.hash));
      if (!chosen.length) return;
      to = chosen[0].hash;
      if (chosen.length > 1) from = chosen[chosen.length - 1].parents[0] ?? EMPTY_TREE;
    } else if (scope.kind === "history") to = scope.commit.hash;
    let live = true;
    setLoading(true);
    api
      .diff(root, from, to)
      .then((d) => live && setPart({ files: parseDiff(d.patch), cut: d.cut }))
      .catch((e) => live && setError(String(e.message ?? e)))
      .finally(() => live && setLoading(false));
    return () => {
      live = false;
    };
  }, [root, scopeKey, info, tick]); // eslint-disable-line react-hooks/exhaustive-deps

  const allByPath = useMemo(() => new Map((all?.files ?? []).map((f) => [f.path, f])), [all]);
  const shown = scope.kind === "all" ? all : part;
  /** The commit the lines shown are from (null: the files as they are now). */
  const rev =
    scope.kind === "commits" ? scope.hashes[0] : scope.kind === "history" ? scope.commit.hash : null;

  // Viewed is about the whole review: a file marked in one commit counts everywhere, until it
  // changes again.
  const stateOf = (path: string) => {
    if (scope.kind === "history") return null;
    const f = allByPath.get(path);
    return f ? viewState(viewed, f) : null;
  };
  const setViewedPath = (path: string, on: boolean) => {
    const f = allByPath.get(path);
    if (!f) return;
    updateViewed((v) => {
      const n = { ...v };
      if (on) n[path] = fingerprint(f);
      else delete n[path];
      return n;
    });
  };
  const viewedCount = (all?.files ?? []).filter((f) => viewState(viewed, f) === "viewed").length;
  const total = all?.files.length ?? 0;

  const hooks: ReviewHooks = {
    viewed: (f) => stateOf(f.path),
    onViewed: (f, on) => setViewedPath(f.path, on),
    comments: comments.filter((c) => c.rev === rev),
    onComment: (c) => updateComments((cs) => [...cs, { ...c, rev, id: `${Date.now()}-${Math.random()}` }]),
    onEdit: (id, text) => updateComments((cs) => cs.map((c) => (c.id === id ? { ...c, text } : c))),
    onDelete: (id) => updateComments((cs) => cs.filter((c) => c.id !== id)),
  };

  const what = !info
    ? "the changes"
    : info.main || !info.commits.length
      ? `the uncommitted changes on \`${info.branch ?? "HEAD"}\``
      : `the branch \`${info.branch ?? "HEAD"}\`${info.parent ? ` (what it adds to \`${info.parent}\`)` : ""}`;

  /** Sends the review; false (and the comments kept) when it can't go now. */
  const send = (summary: string): boolean => {
    const st = store.state;
    if (!st.connected || !st.session) {
      store.toast("Not connected to codeit: the review is kept, send it again in a moment.", true);
      return false;
    }
    if (!modelOf(st)) {
      store.toast("Pick a model first (under the prompt): the review is kept.", true);
      return false;
    }
    store.submit(reviewText(comments, summary, what));
    updateComments(() => []);
    store.toast("Review sent to the agent");
    return true;
  };

  const pick = (hash: string, shift: boolean) => {
    if (!info) return;
    if (!shift || !anchor) {
      setScope({ kind: "commits", hashes: [hash] });
      setAnchor(hash);
      return;
    }
    const a = info.commits.findIndex((c) => c.hash === anchor);
    const b = info.commits.findIndex((c) => c.hash === hash);
    if (a < 0 || b < 0) return;
    const [lo, hi] = a < b ? [a, b] : [b, a];
    setScope({ kind: "commits", hashes: info.commits.slice(lo, hi + 1).map((c) => c.hash) });
  };

  const jump = (path: string) => {
    setScrollTo(null);
    setTimeout(() => setScrollTo(path));
  };

  if (error && !info) return <div className="welcome">{error}</div>;

  const files = shown?.files ?? [];
  const nothing = scope.kind === "all" && all && !all.files.length;
  return (
    <>
      <aside className="wbside review">
        <div className="rsection rfiles">
          <div className="sidetitle">
            Files changed <span className="count">{files.length}</span>
            <span className="spacer" />
            {scope.kind !== "history" && total > 0 && (
              <span className="rprogress" title={`${viewedCount} of ${total} files viewed`}>
                {viewedCount}/{total} viewed
              </span>
            )}
          </div>
          <div className="rlist">
            {files.map((f) => {
              const st = stateOf(f.path);
              const notes = comments.filter((c) => c.path === f.path).length;
              return (
                <div
                  key={f.path}
                  className={`trow file${st === "viewed" ? " viewed" : ""}`}
                  onClick={() => jump(f.path)}
                  title={f.path}
                >
                  {st !== null ? (
                    <input
                      type="checkbox"
                      title={st === "changed" ? "Changed since you viewed it" : "Viewed"}
                      checked={st === "viewed"}
                      onClick={(e) => e.stopPropagation()}
                      onChange={(e) => setViewedPath(f.path, e.target.checked)}
                    />
                  ) : (
                    <span className="nobox" />
                  )}
                  <FileIcon path={f.path} />
                  <span className="tname">{f.path.split("/").pop()}</span>
                  <span className="muted small ellipsis">{f.path.split("/").slice(0, -1).join("/")}</span>
                  <span className="spacer" />
                  {st === "changed" && <span className="gdot" title="Changed since you viewed it" />}
                  {notes > 0 && (
                    <span className="rnotes" title={`${notes} comment${notes > 1 ? "s" : ""}`}>
                      <Icon id="comment" size={12} />
                      {notes}
                    </span>
                  )}
                  <span className="ok small mono">+{f.added}</span>
                  <span className="bad small mono">−{f.removed}</span>
                </div>
              );
            })}
            {shown && !files.length && <div className="muted pad small">No changes.</div>}
          </div>
        </div>

        <div className="rsection rcommits">
          <div className="sidetitle">
            {info?.main ? `On ${info.branch}` : "Commits"}
            {info && !info.main && <span className="count">{info.commits.length}{info.more ? "+" : ""}</span>}
          </div>
          <div className="rlist">
            <div className={`crow${scope.kind === "all" ? " on" : ""}`} onClick={() => setScope({ kind: "all" })}>
              <span className="cmark">
                <Icon id="files" size={14} />
              </span>
              <div className="cbody">
                <div className="csubject">All changes</div>
                <div className="cmeta">
                  {info?.main || !info?.commits.length
                    ? "Uncommitted"
                    : `${info.commits.length} commit${info.commits.length > 1 ? "s" : ""}${info.uncommitted ? " and uncommitted" : ""}`}
                </div>
              </div>
            </div>
            {info?.commits.map((c) => {
              const mine = (c.files ?? []).filter((p) => allByPath.has(p));
              const done = mine.filter((p) => stateOf(p) === "viewed").length;
              const complete = mine.length > 0 && done === mine.length;
              const on = scope.kind === "commits" && scope.hashes.includes(c.hash);
              return (
                <div
                  key={c.hash}
                  className={`crow${on ? " on" : ""}${complete ? " done" : ""}`}
                  onClick={(e) => pick(c.hash, e.shiftKey)}
                  title={`${c.subject}\n\n${c.author} · ${new Date(c.date * 1000).toLocaleString()}\nShift-click picks a range.`}
                >
                  <span className="cmark" title={mine.length ? `${done} of ${mine.length} files viewed` : undefined}>
                    <Icon id={complete ? "pass-filled" : "git-commit"} size={14} />
                  </span>
                  <div className="cbody">
                    <div className="csubject">{c.subject}</div>
                    <div className="cmeta">
                      <span className="mono">{short(c.hash)}</span> · {ago(c.date)}
                      {mine.length > 0 && !complete && (
                        <span>
                          · {done}/{mine.length} viewed
                        </span>
                      )}
                    </div>
                  </div>
                </div>
              );
            })}
            {info && info.uncommitted > 0 && !info.main && info.commits.length > 0 && (
              <div
                className={`crow working${scope.kind === "working" ? " on" : ""}`}
                onClick={() => setScope({ kind: "working" })}
              >
                <span className="cmark">
                  <Icon id="circle-large-outline" size={14} />
                </span>
                <div className="cbody">
                  <div className="csubject">Uncommitted changes</div>
                  <div className="cmeta">{info.uncommitted} files</div>
                </div>
              </div>
            )}
            {info && info.base !== EMPTY_TREE && (
              <History
                root={root}
                from={info.base === "HEAD" ? null : info.base}
                history={history}
                setHistory={setHistory}
                picked={scope.kind === "history" ? scope.commit.hash : null}
                onPick={(c) => setScope({ kind: "history", commit: c })}
              />
            )}
          </div>
        </div>
      </aside>

      <section className="wbmain">
        <div className="diffpane">
          <ReviewHead
            info={info}
            files={all?.files ?? []}
            viewedCount={viewedCount}
            comments={comments}
            loading={loading}
            onRefresh={() => setTick((t) => t + 1)}
            onSend={send}
            onAsk={() => onAsk(`${what.replace(/`/g, "")} `)}
          />
          {scope.kind !== "all" && info && (
            <ScopeBar
              scope={scope}
              info={info}
              files={files}
              onAll={() => setScope({ kind: "all" })}
              onAsk={onAsk}
              onBrowse={(h) => {
                const p = files.find((f) => f.status !== "deleted")?.path;
                if (p) onOpen(p, null, h);
              }}
            />
          )}
          {error && <div className="muted pad">{error}</div>}
          {nothing && info && (
            <div className="welcome">
              <Icon id="pass" size={40} />
              {info.main ? (
                <p>
                  You're on <b>{info.branch}</b> with nothing uncommitted. A review shows what a branch adds: switch to
                  one, or look at a commit under <b>Earlier history</b>.
                </p>
              ) : (
                <p>
                  Nothing to review yet: <b>{info.branch ?? "HEAD"}</b> has no commits or changes of its own.
                </p>
              )}
            </div>
          )}
          {shown && !nothing && (
            <DiffView
              files={shown.files}
              scrollTo={scrollTo}
              review={hooks}
              onAsk={(path, s) => onAsk(reference(path, s, rev))}
              onOpen={(path, line) => onOpen(path, line, rev)}
              onFind={(w, x, y, j) => onFind(w, x, y, j, rev)}
            />
          )}
          {shown?.cut && <div className="muted pad">The diff is too large; it was cut here.</div>}
        </div>
      </section>
    </>
  );
}

function ReviewHead({
  info,
  files,
  viewedCount,
  comments,
  loading,
  onRefresh,
  onSend,
  onAsk,
}: {
  info: ReviewInfo | null;
  files: DiffFile[];
  viewedCount: number;
  comments: Comment[];
  loading: boolean;
  onRefresh: () => void;
  onSend: (summary: string) => boolean;
  onAsk: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [summary, setSummary] = useState("");
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => {
      if (!box.current?.contains(e.target as Node)) setOpen(false);
    };
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [open]);
  const added = files.reduce((n, f) => n + f.added, 0);
  const removed = files.reduce((n, f) => n + f.removed, 0);
  const pct = files.length ? Math.round((viewedCount / files.length) * 100) : 0;
  return (
    <div className="reviewhead">
      <div className="rhtitle">
        <span className="ctitle">Code review</span>
        {info?.branch && (
          <span className="branchchip">
            <Icon id="git-branch" size={13} /> {info.branch}
          </span>
        )}
        {info?.parent && !info.main && info.commits.length > 0 && (
          <span className="muted small">
            since <span className="branchchip subtle">{info.parent}</span>
          </span>
        )}
        {loading && <span className="spinner small" />}
        <span className="spacer" />
        <button className="iconbtn small" title="Look again" onClick={onRefresh}>
          <Icon id="refresh" size={14} />
        </button>
      </div>
      <div className="cline2">
        <span className="small">
          {files.length} files · <span className="ok">+{added}</span> <span className="bad">−{removed}</span>
        </span>
        {files.length > 0 && (
          <span className="rbar" title={`${viewedCount} of ${files.length} files viewed`}>
            <span className="rbartrack">
              <span style={{ width: `${pct}%` }} />
            </span>
            <span className="small muted">
              {viewedCount} / {files.length} viewed
            </span>
          </span>
        )}
        <span className="spacer" />
        <button className="btn small" onClick={onAsk} title="Point the agent at these changes">
          Reference in the prompt
        </button>
        <div className="submitwrap" ref={box}>
          <button className={`btn small${comments.length ? " primary" : ""}`} onClick={() => setOpen(!open)}>
            Submit review{comments.length > 0 && <span className="count">{comments.length}</span>}
            <Icon id="chevron-down" size={12} />
          </button>
          {open && (
            <div className="submitpop">
              <div className="small">
                <b>Finish your review</b>
                <span className="muted">
                  {" "}
                  · {comments.length} comment{comments.length === 1 ? "" : "s"} go to the agent as one message
                </span>
              </div>
              <textarea
                autoFocus
                rows={4}
                placeholder="Overall (optional): what to change, what's fine"
                value={summary}
                onChange={(e) => setSummary(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Escape") setOpen(false);
                }}
              />
              <div className="rcbar">
                <span className="spacer" />
                <button
                  className="btn small primary"
                  disabled={!comments.length && !summary.trim()}
                  onClick={() => {
                    if (!onSend(summary)) return;
                    setSummary("");
                    setOpen(false);
                  }}
                >
                  Send to the agent
                </button>
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

function ScopeBar({
  scope,
  info,
  files,
  onAll,
  onAsk,
  onBrowse,
}: {
  scope: Scope;
  info: ReviewInfo;
  files: DiffFile[];
  onAll: () => void;
  onAsk: (text: string) => void;
  onBrowse: (hash: string) => void;
}) {
  let one: Commit | null = null;
  let title = "";
  let text = "";
  if (scope.kind === "working") {
    title = "Uncommitted changes";
    text = "the uncommitted changes ";
  } else if (scope.kind === "history") {
    one = scope.commit;
  } else if (scope.kind === "commits") {
    const chosen = info.commits.filter((c) => scope.hashes.includes(c.hash));
    if (chosen.length === 1) one = chosen[0];
    else if (chosen.length) {
      title = `${chosen.length} commits`;
      text = `commits ${short(chosen[chosen.length - 1].hash)}..${short(chosen[0].hash)} `;
    }
  }
  if (one) {
    title = one.subject;
    text = `commit ${short(one.hash)} ("${one.subject}") `;
  }
  return (
    <div className={`scopebar${scope.kind === "history" ? " history" : ""}`}>
      <div className="rhtitle">
        <button className="link small" onClick={onAll}>
          <Icon id="arrow-left" size={13} /> All changes
        </button>
        <span className="sbtitle">{title}</span>
      </div>
      {one?.body && <pre className="cbodytext">{one.body}</pre>}
      <div className="cline2">
        {one && (
          <span className="small muted">
            <span className="mono">{short(one.hash)}</span> · {one.author} · {new Date(one.date * 1000).toLocaleString()}
          </span>
        )}
        <span className="small muted">{files.length} files</span>
        {scope.kind === "history" && <span className="small muted">· before this branch: not part of the review</span>}
        <span className="spacer" />
        <button className="btn small" onClick={() => onAsk(text)}>
          Reference in the prompt
        </button>
        {one && (
          <button className="btn small" onClick={() => onBrowse(one.hash)}>
            Browse files here
          </button>
        )}
      </div>
    </div>
  );
}

/** The commits before the review's, on demand. */
function History({
  root,
  from,
  history,
  setHistory,
  picked,
  onPick,
}: {
  root: string;
  from: string | null;
  history: { commits: Commit[]; more: boolean } | null;
  setHistory: (h: { commits: Commit[]; more: boolean } | null) => void;
  picked: string | null;
  onPick: (c: Commit) => void;
}) {
  const [busy, setBusy] = useState(false);
  const more = (skip: number) => {
    setBusy(true);
    api
      .log(root, from, skip)
      .then((l) => setHistory({ commits: [...(skip ? (history?.commits ?? []) : []), ...l.commits], more: l.more }))
      .catch((e) => store.toast(String(e.message ?? e), true))
      .finally(() => setBusy(false));
  };
  if (!history)
    return (
      <button className="historybtn" onClick={() => more(0)} disabled={busy}>
        <Icon id="history" size={14} /> Earlier history {busy && <span className="spinner small" />}
      </button>
    );
  return (
    <>
      <div className="historyhead">
        <Icon id="history" size={13} /> Earlier history
        <span className="spacer" />
        <button className="link small" onClick={() => setHistory(null)}>
          Hide
        </button>
      </div>
      {history.commits.map((c) => (
        <div
          key={c.hash}
          className={`crow old${picked === c.hash ? " on" : ""}`}
          onClick={() => onPick(c)}
          title={`${c.subject}\n\n${c.author} · ${new Date(c.date * 1000).toLocaleString()}`}
        >
          <span className="cmark">
            <Icon id="git-commit" size={14} />
          </span>
          <div className="cbody">
            <div className="csubject">{c.subject}</div>
            <div className="cmeta">
              <span className="mono">{short(c.hash)}</span> · {c.author} · {ago(c.date)}
              {c.refs.map((r) => (
                <span key={r} className="cref">
                  {r.replace(/^HEAD -> /, "")}
                </span>
              ))}
            </div>
          </div>
        </div>
      ))}
      {history.more && (
        <button className="link small pad" onClick={() => more(history.commits.length)} disabled={busy}>
          Older commits
        </button>
      )}
    </>
  );
}
