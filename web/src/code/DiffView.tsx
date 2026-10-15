import { useMemo, useState } from "react";
import type { DiffFile } from "./diff";
import { languageOf } from "./languages";
import { Line, useLineSelection, useTokens, wordAt, type Selection } from "./CodeView";
import { FileIcon, Icon } from "./Icon";

const STATUS: Record<DiffFile["status"], string> = { added: "A", deleted: "D", modified: "M", renamed: "R" };

/** A change's files, one under the other, like a pull request's "Files changed". */
export function DiffView({
  files,
  onAsk,
  onOpen,
  onFind,
  scrollTo,
}: {
  files: DiffFile[];
  onAsk: (path: string, s: Selection) => void;
  onOpen: (path: string, line: number | null) => void;
  onFind: (word: string, x: number, y: number, jump: boolean) => void;
  scrollTo: string | null;
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
}: {
  file: DiffFile;
  big: boolean;
  onAsk: (s: Selection) => void;
  onOpen: (path: string, line: number | null) => void;
  onFind: (word: string, x: number, y: number, jump: boolean) => void;
  target: boolean;
}) {
  const size = file.added + file.removed;
  const [open, setOpen] = useState(!file.binary && file.status !== "deleted" && size < 1500 && !(big && size > 300));
  const firstNew = file.hunks.flatMap((h) => h.lines).find((l) => l.kind === "add")?.new ?? null;
  return (
    <section className="filediff" id={`diff-${file.path}`} ref={(el) => {
        if (target) el?.scrollIntoView({ block: "start" });
      }}>
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
        <span className="spacer" />
        {file.added > 0 && <span className="ok mono small">+{file.added}</span>}
        {file.removed > 0 && <span className="bad mono small">−{file.removed}</span>}
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
      </header>
      {open && (file.binary ? <div className="muted pad">Binary file</div> : <Hunks file={file} onAsk={onAsk} onFind={onFind} />)}
      {!open && !file.binary && (
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
}: {
  file: DiffFile;
  onAsk: (s: Selection) => void;
  onFind: (word: string, x: number, y: number, jump: boolean) => void;
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
            const on = sel && n !== null && n >= sel.from && n <= sel.to;
            const last = sel && n === sel.to;
            return (
              <div key={li}>
                <div className={`dline ${l.kind}${on ? " sel" : ""}`}>
                  <span className="ln old">{l.old ?? ""}</span>
                  <span
                    className="ln"
                    onMouseDown={(e) => n !== null && down(n, e)}
                    onMouseEnter={() => n !== null && enter(n)}
                  >
                    {n ?? ""}
                  </span>
                  <span className="mk">{l.kind === "add" ? "+" : l.kind === "del" ? "−" : " "}</span>
                  <span className="ct">
                    <Line toks={toks ?? [{ t: l.text }]} />
                  </span>
                </div>
                {last && sel && (
                  <div className="selbar inline">
                    <span className="muted">
                      {sel.from === sel.to ? `Line ${sel.from}` : `Lines ${sel.from}–${sel.to}`}
                    </span>
                    <button className="btn small primary" onClick={() => onAsk(sel)}>
                      Reference in the prompt
                    </button>
                    <button className="iconbtn small" title="Clear (esc)" onClick={() => setSel(null)}>
                      ✕
                    </button>
                  </div>
                )}
              </div>
            );
          })}
        </div>
      ))}
    </div>
  );
}
