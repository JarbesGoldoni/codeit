import { useEffect, useMemo, useRef, useState } from "react";
import type { DiffFile } from "./diff";
import { languageOf } from "./languages";
import { Line, useLineSelection, useTokens, wordAt, type Selection } from "./CodeView";
import { FileIcon, Icon } from "./Icon";
import { quoteOf, type Comment, type ViewState } from "./review";

const STATUS: Record<DiffFile["status"], string> = { added: "A", deleted: "D", modified: "M", renamed: "R" };

/** What a code review adds to the diff: files marked viewed, and comments. */
export interface ReviewHooks {
  /** null: the file isn't part of the review (it can't be marked). */
  viewed: (f: DiffFile) => ViewState | null;
  onViewed: (f: DiffFile, on: boolean) => void;
  /** The comments on these files at this version. */
  comments: Comment[];
  onComment: (c: Omit<Comment, "id" | "rev">) => void;
  onEdit: (id: string, text: string) => void;
  onDelete: (id: string) => void;
}

/** A change's files, one under the other, like a pull request's "Files changed". */
export function DiffView({
  files,
  onAsk,
  onOpen,
  onFind,
  scrollTo,
  review,
}: {
  files: DiffFile[];
  onAsk: (path: string, s: Selection) => void;
  onOpen: (path: string, line: number | null) => void;
  onFind: (word: string, x: number, y: number, jump: boolean) => void;
  scrollTo: string | null;
  review?: ReviewHooks;
}) {
  if (!files.length) return <div className="muted pad">No changes.</div>;
  return (
    <div className="difffiles">
      {files.map((f) => (
        <FileDiff
          key={`${f.oldPath ?? ""}>${f.path}`}
          file={f}
          big={files.length > 40}
          onAsk={(s) => onAsk(f.path, s)}
          onOpen={onOpen}
          onFind={onFind}
          target={scrollTo === f.path}
          review={review}
        />
      ))}
    </div>
  );
}

function FileDiff({
  file,
  big,
  onAsk,
  onOpen,
  onFind,
  target,
  review,
}: {
  file: DiffFile;
  big: boolean;
  onAsk: (s: Selection) => void;
  onOpen: (path: string, line: number | null) => void;
  onFind: (word: string, x: number, y: number, jump: boolean) => void;
  target: boolean;
  review?: ReviewHooks;
}) {
  const size = file.added + file.removed;
  const state = review?.viewed(file) ?? null;
  const [open, setOpen] = useState(
    state !== "viewed" && !file.binary && file.status !== "deleted" && size < 1500 && !(big && size > 300),
  );
  // Marked viewed, it folds away; unmarked or changed since, it opens again.
  const was = useRef(state);
  useEffect(() => {
    if (was.current !== state) setOpen(state !== "viewed");
    was.current = state;
  }, [state]);
  const [fileForm, setFileForm] = useState(false);
  const firstNew = file.hunks.flatMap((h) => h.lines).find((l) => l.kind === "add")?.new ?? null;
  const mine = review?.comments.filter((c) => c.path === file.path) ?? [];
  // Shown at the top: on the whole file, or on lines this diff doesn't show (any more).
  const shownLines = useMemo(
    () => new Set(file.hunks.flatMap((h) => h.lines.filter((l) => l.kind !== "del").map((l) => l.new))),
    [file],
  );
  const top = mine.filter((c) => c.to === null || !shownLines.has(c.to));
  return (
    <section
      className={`filediff${state === "viewed" ? " viewed" : ""}`}
      id={`diff-${file.path}`}
      ref={(el) => {
        if (target) el?.scrollIntoView({ block: "start" });
      }}
    >
      <header className="filehead" onClick={() => setOpen(!open)}>
        <span className={`chev${open ? " open" : ""}`}>
          <Icon id="chevron-right" size={14} />
        </span>
        <FileIcon path={file.path} />
        <span className="fpath" title={file.path}>
          {file.oldPath && <span className="muted">{file.oldPath} → </span>}
          {file.path}
        </span>
        <span className={`fstatus s-${file.status}`}>{STATUS[file.status]}</span>
        {state === "changed" && <span className="fchanged">Changed since viewed</span>}
        <span className="spacer" />
        {file.added > 0 && <span className="ok mono small">+{file.added}</span>}
        {file.removed > 0 && <span className="bad mono small">−{file.removed}</span>}
        {review && (
          <button
            className="iconbtn small"
            title="Comment on the file"
            onClick={(e) => {
              e.stopPropagation();
              setOpen(true);
              setFileForm(true);
            }}
          >
            <Icon id="comment" size={15} />
          </button>
        )}
        {file.status !== "deleted" && (
          <button
            className="iconbtn small"
            title="Open the file"
            onClick={(e) => {
              e.stopPropagation();
              onOpen(file.path, firstNew);
            }}
          >
            <Icon id="go-to-file" size={15} />
          </button>
        )}
        {review && state !== null && (
          <label className={`viewedbox${state === "viewed" ? " on" : ""}`} onClick={(e) => e.stopPropagation()}>
            <input
              type="checkbox"
              checked={state === "viewed"}
              onChange={(e) => review.onViewed(file, e.target.checked)}
            />
            Viewed
          </label>
        )}
      </header>
      {open && review && (top.length > 0 || fileForm) && (
        <div className="filecomments">
          {top.map((c) => (
            <CommentBox key={c.id} c={c} review={review} />
          ))}
          {fileForm && (
            <CommentForm
              placeholder="A comment on the whole file"
              onCancel={() => setFileForm(false)}
              onSave={(text) => {
                review.onComment({ path: file.path, from: null, to: null, quote: "", text });
                setFileForm(false);
              }}
            />
          )}
        </div>
      )}
      {open &&
        (file.binary ? (
          <div className="muted pad">Binary file</div>
        ) : (
          <Hunks file={file} onAsk={onAsk} onFind={onFind} review={review} comments={mine} />
        ))}
      {!open && !file.binary && state !== "viewed" && (
        <button className="link small pad" onClick={() => setOpen(true)}>
          Show {size} changed lines
        </button>
      )}
    </section>
  );
}

function Hunks({
  file,
  onAsk,
  onFind,
  review,
  comments,
}: {
  file: DiffFile;
  onAsk: (s: Selection) => void;
  onFind: (word: string, x: number, y: number, jump: boolean) => void;
  review?: ReviewHooks;
  comments: Comment[];
}) {
  const lang = languageOf(file.path);
  // Each side's lines as one text, so the grammar sees them in order.
  const { newText, oldText } = useMemo(() => {
    const n: string[] = [];
    const o: string[] = [];
    for (const h of file.hunks)
      for (const l of h.lines) {
        if (l.kind !== "del") n.push(l.text);
        if (l.kind !== "add") o.push(l.text);
      }
    return { newText: n.join("\n"), oldText: o.join("\n") };
  }, [file]);
  const newToks = useTokens(newText, lang);
  const oldToks = useTokens(oldText, lang);
  const { sel, setSel, down, enter } = useLineSelection();
  const [drafting, setDrafting] = useState<Selection | null>(null);
  const byLine = useMemo(() => {
    const m = new Map<number, Comment[]>();
    for (const c of comments) if (c.to !== null) m.set(c.to, [...(m.get(c.to) ?? []), c]);
    return m;
  }, [comments]);
  let ni = 0;
  let oi = 0;
  return (
    <div
      className="hunks"
      onKeyDown={(e) => e.key === "Escape" && setSel(null)}
      tabIndex={-1}
      onClick={(e) => {
        if (!window.getSelection()?.isCollapsed) return;
        if (!(e.target as HTMLElement).closest(".ct")) return;
        const w = wordAt(e.clientX, e.clientY);
        if (w) onFind(w, e.clientX, e.clientY, e.metaKey || e.ctrlKey);
      }}
    >
      {file.hunks.map((h, hi) => (
        <div key={hi}>
          <div className="hunkhead">
            @@ −{h.oldStart} +{h.newStart} @@ <span className="muted">{h.header}</span>
          </div>
          {h.lines.map((l, li) => {
            const toks = l.kind === "del" ? oldToks[oi++] : newToks[ni++];
            if (l.kind === "ctx") oi++;
            const n = l.new;
            const on = (sel && n !== null && n >= sel.from && n <= sel.to) || (drafting && n !== null && n >= drafting.from && n <= drafting.to);
            const last = sel && n === sel.to;
            const notes = n !== null && l.kind !== "del" ? byLine.get(n) : undefined;
            return (
              <div key={li}>
                <div className={`dline ${l.kind}${on ? " sel" : ""}`}>
                  <span className="ln old">{l.old ?? ""}</span>
                  <span
                    className="ln"
                    title={n !== null && review ? "Click to comment on this line (shift-click or drag for more)" : undefined}
                    onMouseDown={(e) => {
                      if (n === null) return;
                      setDrafting(null);
                      down(n, e);
                    }}
                    onMouseEnter={() => n !== null && enter(n)}
                  >
                    {n ?? ""}
                  </span>
                  <span className="mk">{l.kind === "add" ? "+" : l.kind === "del" ? "−" : " "}</span>
                  <span className="ct">
                    <Line toks={toks ?? [{ t: l.text }]} />
                  </span>
                </div>
                {notes && review && notes.map((c) => <CommentBox key={c.id} c={c} review={review} />)}
                {last && sel && !drafting && (
                  <div className="selbar inline">
                    <span className="muted">
                      {sel.from === sel.to ? `Line ${sel.from}` : `Lines ${sel.from}–${sel.to}`}
                    </span>
                    {review && (
                      <button className="btn small primary" onClick={() => setDrafting(sel)}>
                        <Icon id="comment" size={13} /> Comment
                      </button>
                    )}
                    <button className={`btn small${review ? "" : " primary"}`} onClick={() => onAsk(sel)}>
                      Reference in the prompt
                    </button>
                    <button className="iconbtn small" title="Clear (esc)" onClick={() => setSel(null)}>
                      ✕
                    </button>
                  </div>
                )}
                {drafting && review && n === drafting.to && l.kind !== "del" && (
                  <CommentForm
                    placeholder={drafting.from === drafting.to ? `Comment on line ${drafting.from}` : `Comment on lines ${drafting.from}–${drafting.to}`}
                    onCancel={() => {
                      setDrafting(null);
                      setSel(null);
                    }}
                    onSave={(text) => {
                      review.onComment({
                        path: file.path,
                        from: drafting.from,
                        to: drafting.to,
                        quote: quoteOf(file, drafting.from, drafting.to),
                        text,
                      });
                      setDrafting(null);
                      setSel(null);
                    }}
                  />
                )}
              </div>
            );
          })}
        </div>
      ))}
    </div>
  );
}

function CommentBox({ c, review }: { c: Comment; review: ReviewHooks }) {
  const [editing, setEditing] = useState(false);
  if (editing)
    return (
      <CommentForm
        initial={c.text}
        onCancel={() => setEditing(false)}
        onSave={(text) => {
          review.onEdit(c.id, text);
          setEditing(false);
        }}
      />
    );
  return (
    <div className="rcomment">
      <div className="rchead">
        <Icon id="comment" size={13} />
        <span className="muted small">
          {c.from === null ? "On the file" : c.from === c.to ? `Line ${c.from}` : `Lines ${c.from}–${c.to}`} · pending
        </span>
        <span className="spacer" />
        <button className="link small" onClick={() => setEditing(true)}>
          Edit
        </button>
        <button className="link small" onClick={() => review.onDelete(c.id)}>
          Delete
        </button>
      </div>
      <div className="rctext">{c.text}</div>
    </div>
  );
}

function CommentForm({
  initial = "",
  placeholder,
  onSave,
  onCancel,
}: {
  initial?: string;
  placeholder?: string;
  onSave: (text: string) => void;
  onCancel: () => void;
}) {
  const [text, setText] = useState(initial);
  const save = () => text.trim() && onSave(text.trim());
  return (
    <div className="rcomment form" onClick={(e) => e.stopPropagation()}>
      <textarea
        autoFocus
        rows={3}
        value={text}
        placeholder={placeholder ?? "Leave a comment"}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) save();
          if (e.key === "Escape") onCancel();
        }}
      />
      <div className="rcbar">
        <span className="muted small">Sent with the review · ⌘/ctrl+enter</span>
        <span className="spacer" />
        <button className="btn small" onClick={onCancel}>
          Cancel
        </button>
        <button className="btn small primary" disabled={!text.trim()} onClick={save}>
          {initial ? "Save" : "Add comment"}
        </button>
      </div>
    </div>
  );
}
