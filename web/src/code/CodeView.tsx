import { useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { tokenize, type Tok } from "./highlight";
import { useSkin } from "./skin";

export const LINE = 20;

const dark = window.matchMedia("(prefers-color-scheme: dark)");

/** The highlighting theme: the skin's when its colors are on, else light or dark. */
export function useThemeKey(): string {
  const skin = useSkin();
  const isDark = useSyncExternalStore(
    (l) => {
      dark.addEventListener("change", l);
      return () => dark.removeEventListener("change", l);
    },
    () => dark.matches,
  );
  return skin.colors ? "skin" : isDark ? "dark" : "light";
}

/** The text's lines, colored once the grammar is loaded (plain until then). */
export function useTokens(text: string, lang: string): Tok[][] {
  const theme = useThemeKey();
  const plain = useMemo(() => text.split("\n").map((t) => [{ t }]), [text]);
  const [lines, setLines] = useState<{ text: string; theme: string; lines: Tok[][] } | null>(null);
  useEffect(() => {
    let live = true;
    tokenize(text, lang, theme).then((l) => live && setLines({ text, theme, lines: l }));
    return () => {
      live = false;
    };
  }, [text, lang, theme]);
  return lines && lines.text === text && lines.theme === theme ? lines.lines : plain;
}

export function Line({ toks }: { toks: Tok[] }) {
  return (
    <>
      {toks.map((k, i) =>
        k.c || k.s ? (
          <span
            key={i}
            style={{
              color: k.c,
              fontStyle: k.s && k.s & 1 ? "italic" : undefined,
              fontWeight: k.s && k.s & 2 ? 600 : undefined,
              textDecoration: k.s && k.s & 4 ? "underline" : undefined,
            }}
          >
            {k.t}
          </span>
        ) : (
          <span key={i}>{k.t}</span>
        ),
      )}
      {"\n"}
    </>
  );
}

/** The identifier under a click, if any. */
export function wordAt(x: number, y: number): string | null {
  let node: Node | null = null;
  let offset = 0;
  const d = document as Document & {
    caretPositionFromPoint?: (x: number, y: number) => { offsetNode: Node; offset: number } | null;
  };
  if (d.caretPositionFromPoint) {
    const p = d.caretPositionFromPoint(x, y);
    if (p) [node, offset] = [p.offsetNode, p.offset];
  } else if (document.caretRangeFromPoint) {
    const r = document.caretRangeFromPoint(x, y);
    if (r) [node, offset] = [r.startContainer, r.startOffset];
  }
  if (!node || node.nodeType !== Node.TEXT_NODE) return null;
  const w = /[\w$]/;
  // Right after a word, the caret is at the start of the next piece: take the word before.
  const prev = node.parentElement?.previousElementSibling?.firstChild;
  if (offset === 0 && !w.test(node.textContent?.[0] ?? "") && prev?.nodeType === Node.TEXT_NODE) {
    node = prev;
    offset = prev.textContent?.length ?? 0;
  }
  const text = node.textContent ?? "";
  let a = offset;
  let b = offset;
  while (a > 0 && w.test(text[a - 1])) a--;
  while (b < text.length && w.test(text[b])) b++;
  const word = text.slice(a, b);
  return /^[A-Za-z_$][\w$]*$/.test(word) && word.length > 1 ? word : null;
}

export interface Selection {
  from: number;
  to: number;
}

/** Line selection by the gutter: click, shift-click or drag. */
export function useLineSelection() {
  const [sel, setSel] = useState<(Selection & { anchor: number }) | null>(null);
  const dragging = useRef(false);
  useEffect(() => {
    const up = () => (dragging.current = false);
    window.addEventListener("mouseup", up);
    return () => window.removeEventListener("mouseup", up);
  }, []);
  const down = (n: number, e: React.MouseEvent) => {
    e.preventDefault();
    dragging.current = true;
    if (e.shiftKey && sel) setSel({ anchor: sel.anchor, from: Math.min(sel.anchor, n), to: Math.max(sel.anchor, n) });
    else if (sel && sel.from === n && sel.to === n) setSel(null);
    else setSel({ anchor: n, from: n, to: n });
  };
  const enter = (n: number) => {
    if (dragging.current && sel) setSel({ anchor: sel.anchor, from: Math.min(sel.anchor, n), to: Math.max(sel.anchor, n) });
  };
  return { sel, setSel, down, enter };
}

/** A file's code, read-only: line numbers to select lines, identifiers to look up. */
export function CodeView({
  text,
  lang,
  reveal,
  onAsk,
  onFind,
}: {
  text: string;
  lang: string;
  /** Scroll to this line and mark it (changes with `reveal.at`). */
  reveal: { line: number; at: number } | null;
  onAsk: (s: Selection) => void;
  onFind: (word: string, x: number, y: number, jump: boolean) => void;
}) {
  const lines = useTokens(text, lang);
  const box = useRef<HTMLDivElement>(null);
  const [view, setView] = useState({ top: 0, height: 800 });
  const [mark, setMark] = useState<number | null>(null);
  const { sel, setSel, down, enter } = useLineSelection();
  const digits = String(lines.length).length;

  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    const measure = () => setView({ top: el.scrollTop, height: el.clientHeight });
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  useEffect(() => {
    setSel(null);
    // A new file starts at the top unless a line is asked for.
    if (!reveal && box.current) box.current.scrollTop = 0;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [text]);

  useEffect(() => {
    const el = box.current;
    if (!reveal || !el) return;
    el.scrollTop = Math.max(0, (reveal.line - 1) * LINE - el.clientHeight / 3);
    setMark(reveal.line);
    const t = setTimeout(() => setMark(null), 2400);
    return () => clearTimeout(t);
  }, [reveal?.at, text]); // eslint-disable-line react-hooks/exhaustive-deps

  const first = Math.max(0, Math.floor(view.top / LINE) - 30);
  const last = Math.min(lines.length, Math.ceil((view.top + view.height) / LINE) + 30);

  return (
    <div
      className="codeview"
      ref={box}
      tabIndex={0}
      onScroll={(e) => setView({ top: e.currentTarget.scrollTop, height: e.currentTarget.clientHeight })}
      onKeyDown={(e) => {
        if (e.key === "Escape") setSel(null);
      }}
      style={{ ["--gutter" as string]: `${digits + 3}ch` }}
    >
      <div className="codeinner" style={{ height: lines.length * LINE + 40 }}>
        <div className="coderows" style={{ transform: `translateY(${first * LINE}px)` }}>
          {lines.slice(first, last).map((toks, i) => {
            const n = first + i + 1;
            const on = sel && n >= sel.from && n <= sel.to;
            return (
              <div key={n} className={`cline${on ? " sel" : ""}${mark === n ? " mark" : ""}`}>
                <span className="ln" onMouseDown={(e) => down(n, e)} onMouseEnter={() => enter(n)}>
                  {n}
                </span>
                <span
                  className="ct"
                  onClick={(e) => {
                    if (!window.getSelection()?.isCollapsed) return;
                    const w = wordAt(e.clientX, e.clientY);
                    if (w) onFind(w, e.clientX, e.clientY, e.metaKey || e.ctrlKey);
                  }}
                >
                  <Line toks={toks} />
                </span>
              </div>
            );
          })}
        </div>
        {sel && (
          <div className="selbar" style={{ top: sel.to * LINE + 4 }}>
            <span className="muted">{sel.from === sel.to ? `Line ${sel.from}` : `Lines ${sel.from}–${sel.to}`}</span>
            <button className="btn small primary" onClick={() => onAsk(sel)}>
              Reference in the prompt
            </button>
            <button
              className="btn small"
              onClick={() => {
                const chosen = text.split("\n").slice(sel.from - 1, sel.to).join("\n");
                navigator.clipboard?.writeText(chosen);
              }}
            >
              Copy
            </button>
            <button className="iconbtn small" title="Clear (esc)" onClick={() => setSel(null)}>
              ✕
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
