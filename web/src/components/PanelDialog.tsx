import { useEffect, useRef, useState } from "react";
import { store, useStore } from "../store";

/** A plugin's panel: its sections and rows, driven by the plugin's answers to what you press. */
export function PanelDialog() {
  const p = useStore((s) => s.panel);
  const [sel, setSel] = useState(0);
  const [text, setText] = useState("");
  const box = useRef<HTMLDivElement>(null);
  const panel = p?.panel ?? null;
  const ids = panel ? panel.sections.flatMap((s) => s.rows.map((r) => r.id)).filter((x): x is string => !!x) : [];
  const selected = ids[Math.min(sel, ids.length - 1)] ?? null;

  // Follow the row the plugin asks to select.
  useEffect(() => {
    if (panel?.select) {
      const i = ids.indexOf(panel.select);
      if (i >= 0) setSel(i);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [panel]);

  // Poll while the plugin asks to (a login waiting for approval).
  useEffect(() => {
    if (!panel?.poll_ms || p?.input) return;
    const t = setTimeout(() => store.panelAct({ kind: "poll" }), panel.poll_ms);
    return () => clearTimeout(t);
  }, [panel, p?.input]);

  useEffect(() => {
    setText("");
    if (!p?.input) box.current?.focus();
  }, [p?.input, p?.command]);

  useEffect(() => {
    box.current?.querySelector(".prow.on")?.scrollIntoView({ block: "nearest" });
  }, [sel]);

  if (!p) return null;
  const close = () => store.closePanel();

  const onKey = (e: React.KeyboardEvent) => {
    if (p.input) return;
    if (e.key === "Escape") {
      e.preventDefault();
      close();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      setSel((s) => Math.min(s + 1, ids.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSel((s) => Math.max(s - 1, 0));
    } else if (e.key === "Enter" && selected) {
      e.preventDefault();
      store.panelAct({ kind: "enter", id: selected });
    } else if (panel?.keys.some((k) => k.key === e.key) && !e.metaKey && !e.ctrlKey && !e.altKey) {
      e.preventDefault();
      store.panelAct({ kind: "key", key: e.key, id: selected });
    }
  };

  return (
    <div className="overlay" onMouseDown={close}>
      <div className="dialog panel" ref={box} tabIndex={-1} onKeyDown={onKey} onMouseDown={(e) => e.stopPropagation()}>
        <div className="dialoghead">
          <span>{panel?.title ?? `/${p.command}`}</span>
          {p.plugin && p.plugin !== "codeit" && <span className="pluginbadge">{p.plugin}</span>}
          {p.busy && <span className="spinner small" />}
          <span className="spacer" />
          <button className="iconbtn" onClick={close} title="Close (esc)">
            ✕
          </button>
        </div>
        {panel?.message && <div className={`pmessage tone-${panel.message.tone}`}>{panel.message.text}</div>}
        {p.input ? (
          <form
            className="dialogbody"
            onSubmit={(e) => {
              e.preventDefault();
              store.panelAct({ kind: "text", text });
            }}
          >
            <div className="ptitle">{p.input.title}</div>
            <p className="muted">{p.input.hint}</p>
            {/* A textarea even for secrets: pasted cookies come one per line. Masked with CSS. */}
            <textarea
              autoFocus
              rows={p.input.secret ? 3 : 4}
              className={p.input.secret ? "secret" : undefined}
              spellCheck={false}
              autoComplete="off"
              value={text}
              onChange={(e) => setText(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Escape") store.panelAct({ kind: "text", text: "" });
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  store.panelAct({ kind: "text", text });
                }
              }}
            />
            <div className="row">
              <button className="btn primary">Send</button>
              <button type="button" className="btn" onClick={() => store.panelAct({ kind: "text", text: "" })}>
                Back
              </button>
            </div>
          </form>
        ) : (
          <div className="dialogbody plist">
            {!panel && <div className="muted pad">Loading…</div>}
            {panel?.sections.map((s, si) => (
              <div key={si} className="psection">
                {s.title && <div className="pgroup">{s.title}</div>}
                {s.rows.map((r, ri) => {
                  const idx = r.id ? ids.indexOf(r.id) : -1;
                  const on = r.id !== null && r.id === selected;
                  const content = (
                    <>
                      <div className="pline">
                        <span className={`plabel tone-${r.tone}`}>{r.label}</span>
                        <span className={`pvalue tone-${r.tone}`}>{r.value}</span>
                      </div>
                      {r.bar !== null && (
                        <div className="meter">
                          <span style={{ width: `${Math.round(r.bar * 100)}%` }} className={`tone-bg-${r.tone}`} />
                        </div>
                      )}
                      {r.detail.map((d, di) => (
                        <div key={di} className="pdetailline">
                          {d}
                        </div>
                      ))}
                    </>
                  );
                  return r.id ? (
                    <button
                      key={ri}
                      className={`prow${on ? " on" : ""}`}
                      onMouseEnter={() => setSel(idx)}
                      onClick={() => store.panelAct({ kind: "enter", id: r.id! })}
                    >
                      {content}
                    </button>
                  ) : (
                    <div key={ri} className="prow info">
                      {content}
                    </div>
                  );
                })}
              </div>
            ))}
          </div>
        )}
        {panel && !p.input && (
          <div className="pkeys">
            <span>
              <kbd>↑↓</kbd> move
            </span>
            <span>
              <kbd>enter</kbd> open
            </span>
            {panel.keys.map((k) => (
              <button key={k.key} className="keybtn" onClick={() => store.panelAct({ kind: "key", key: k.key, id: selected })}>
                <kbd>{k.key}</kbd> {k.label}
              </button>
            ))}
            <span>
              <kbd>esc</kbd> close
            </span>
          </div>
        )}
      </div>
    </div>
  );
}
