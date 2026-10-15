import { useEffect, useMemo, useRef, useState } from "react";
import { modelOf, speechEngine, store, useStore } from "../store";
import { listen, type Recording } from "../voice";
import { tokens } from "../format";
import { Asks } from "./Asks";

function Caret() {
  return (
    <svg className="caret" viewBox="0 0 10 10" width="9" height="9" aria-hidden>
      <path d="M2 3.5l3 3 3-3" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

function MicIcon() {
  return (
    <svg viewBox="0 0 24 24" width="17" height="17" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
      <rect x="9" y="3" width="6" height="11" rx="3" />
      <path d="M5 11a7 7 0 0 0 14 0M12 18v3" />
    </svg>
  );
}

function SendIcon() {
  return (
    <svg viewBox="0 0 24 24" width="17" height="17" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M12 19V5M5 12l7-7 7 7" />
    </svg>
  );
}

function StopIcon() {
  return (
    <svg viewBox="0 0 24 24" width="15" height="15">
      <rect x="6" y="6" width="12" height="12" rx="2" fill="currentColor" />
    </svg>
  );
}

type VoiceState = { kind: "idle" } | { kind: "recording"; rec: Recording; since: number } | { kind: "working" };

/** Speech to text: click (or ctrl+shift+space) to start, again to stop and transcribe. */
function useVoice(insert: (text: string) => void) {
  const [state, setState] = useState<VoiceState>({ kind: "idle" });
  const [level, setLevel] = useState(0);
  // What the browser's speech service has heard so far (Whisper gives text only at the end).
  const [partial, setPartial] = useState("");
  const stateRef = useRef(state);
  stateRef.current = state;

  useEffect(() => {
    if (state.kind !== "recording") return;
    let raf = 0;
    const loop = () => {
      setLevel(state.rec.level());
      raf = requestAnimationFrame(loop);
    };
    loop();
    return () => cancelAnimationFrame(raf);
  }, [state]);

  const toggle = async () => {
    const s = stateRef.current;
    if (s.kind === "working") return;
    if (s.kind === "idle") {
      const engine = speechEngine(store.state);
      if (!engine) {
        store.setOverlay({ kind: "voice" });
        return;
      }
      setPartial("");
      try {
        const rec = await listen(engine, store.state.voice?.language ?? "auto", {
          partial: setPartial,
          broke: (e) => {
            store.toast(e.message, true);
            setPartial("");
            setState({ kind: "idle" });
          },
        });
        setState({ kind: "recording", rec, since: Date.now() });
      } catch (e) {
        store.toast(`Microphone: ${e instanceof Error ? e.message : String(e)}`, true);
      }
      return;
    }
    setState({ kind: "working" });
    try {
      const text = await s.rec.stop();
      if (text) insert(text);
      else store.toast("Didn't catch anything.");
    } catch (e) {
      store.toast(e instanceof Error ? e.message : String(e), true);
    }
    setPartial("");
    setState({ kind: "idle" });
  };

  const cancel = () => {
    const s = stateRef.current;
    if (s.kind === "recording") {
      s.rec.cancel();
      setPartial("");
      setState({ kind: "idle" });
    }
  };

  return { state, level, partial, toggle, cancel };
}

function CommandMenu({
  query,
  selected,
  onPick,
}: {
  query: string;
  selected: number;
  onPick: (name: string) => void;
}) {
  const matches = useMatches(query);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    ref.current?.querySelector(".on")?.scrollIntoView({ block: "nearest" });
  }, [selected]);
  if (matches.length === 0) return null;
  const current = matches[Math.min(selected, matches.length - 1)];
  return (
    <div className="cmdmenu" ref={ref}>
      <div className="cmdlist">
        {matches.map((c, i) => (
          <button
            key={c.name}
            className={`cmd${i === selected ? " on" : ""}`}
            onMouseDown={(e) => {
              e.preventDefault();
              onPick(c.name);
            }}
          >
            <span className="cmdname">{c.name}</span>
            <span className="cmdargs">{c.args}</span>
            <span className="cmdhelp">{c.help}</span>
            {c.source !== "codeit" && c.source !== "prompt" && <span className="pluginbadge">{c.source}</span>}
          </button>
        ))}
      </div>
      {current?.long && <div className="cmdlong">{current.long}</div>}
    </div>
  );
}

function useMatches(query: string) {
  const commands = useStore((s) => s.commands);
  return useMemo(() => {
    const q = query.toLowerCase();
    const starts = commands.filter((c) => c.name.slice(1).toLowerCase().startsWith(q));
    const has = commands.filter((c) => !starts.includes(c) && c.name.toLowerCase().includes(q));
    return [...starts, ...has];
  }, [commands, query]);
}

/** At this share of the context, a note suggests /compact: past it, every message resends a
 * long conversation (and many models charge more for a long context). */
const CONTEXT_NOTE = 0.2;

function ContextNotice() {
  const id = useStore((s) => s.session?.id ?? null);
  const context = useStore((s) => s.session?.context ?? null);
  const busy = useStore((s) => s.session?.busy ?? false);
  const [dismissed, setDismissed] = useState<string[]>(() => {
    try {
      return JSON.parse(localStorage.getItem("codeit.ctxnote") ?? "[]");
    } catch {
      return [];
    }
  });
  const share = context ? context[0] / context[1] : 0;
  const keep = (list: string[]) => {
    setDismissed(list);
    try {
      localStorage.setItem("codeit.ctxnote", JSON.stringify(list.slice(-200)));
    } catch {
      // For this tab only.
    }
  };
  // Compacted (or undone) below it: the note may come back later.
  useEffect(() => {
    if (id && context && share < CONTEXT_NOTE && dismissed.includes(id)) keep(dismissed.filter((d) => d !== id));
  }, [id, share]); // eslint-disable-line react-hooks/exhaustive-deps
  if (!id || !context || share < CONTEXT_NOTE || dismissed.includes(id)) return null;
  return (
    <div className="ctxnote">
      <div>
        <b>{Math.round(share * 100)}% of the context is used</b>{" "}
        <span className="muted">
          ({tokens(context[0])} of {tokens(context[1])} tokens)
        </span>
        . Each message resends all of it, so it costs more from here. <code>/compact</code> sums it up and carries on
        lighter.
      </div>
      <div className="ctxnotebtns">
        <button
          className="btn small primary"
          disabled={busy}
          title={busy ? "After this turn" : undefined}
          onClick={() => store.submit("/compact")}
        >
          Compact now
        </button>
        <button className="btn small" onClick={() => keep([...dismissed, id])}>
          Not now
        </button>
      </div>
    </div>
  );
}

function Toolbar({ voice }: { voice: ReturnType<typeof useVoice> }) {
  const session = useStore((s) => s.session);
  const agents = useStore((s) => s.agents);
  const model = useStore(modelOf);
  const engine = useStore(speechEngine);
  const loading = useStore((s) => s.models.loading);
  if (!session) return null;
  const { settings, context } = session;
  const nextAgent = () => {
    const names = agents.map((a) => a.name);
    const next = names[(names.indexOf(settings.agent) + 1) % names.length];
    if (next) store.send({ type: "agent", session: session.id, agent: next });
  };
  const ctx = context ? Math.min(100, Math.round((context[0] / context[1]) * 100)) : null;
  const rec = voice.state.kind === "recording";
  return (
    <div className="toolbar">
      <button className={`pill agent ${settings.agent}`} onClick={nextAgent} title="Tab switches agent">
        {settings.agent}
      </button>
      <button className="pill" onClick={() => store.setOverlay({ kind: "models" })}>
        {model?.name ?? (settings.model ? settings.model.split("/")[1] : loading ? "loading models…" : "pick a model")}
        <Caret />
      </button>
      {model && model.efforts.length > 0 && (
        <button className="pill" onClick={() => store.setOverlay({ kind: "effort" })} title="Reasoning effort">
          {settings.effort ?? "default"}
          <Caret />
        </button>
      )}
      <button
        className={`pill subtle${settings.approval === "ask" ? " on" : ""}`}
        onClick={() =>
          store.send({ type: "approval", session: session.id, approval: settings.approval === "ask" ? "auto" : "ask" })
        }
        title={settings.approval === "ask" ? "Asks before edits and commands" : "Runs tools without asking (except outside the project)"}
      >
        {settings.approval === "ask" ? "ask first" : "auto"}
      </button>
      <span className="spacer" />
      {ctx !== null && context && (
        <span className="ctx" title={`${tokens(context[0])} of ${tokens(context[1])} tokens of context`}>
          <span className="ctxbar">
            <span style={{ width: `${ctx}%` }} className={ctx > 85 ? "hot" : ""} />
          </span>
          {ctx}%
        </span>
      )}
      <button
        className={`iconbtn mic${rec ? " rec" : ""}${voice.state.kind === "working" ? " busy" : ""}`}
        onClick={() => void voice.toggle()}
        title={
          engine
            ? rec
              ? "Stop (ctrl+shift+space)"
              : `Talk (ctrl+shift+space), with ${engine === "browser" ? "the browser's speech service" : "Whisper"}`
            : "Speech to text isn't set up here: click for how"
        }
        style={rec ? { boxShadow: `0 0 0 ${2 + voice.level * 10}px var(--rec-glow)` } : undefined}
      >
        {voice.state.kind === "working" ? <span className="spinner" /> : <MicIcon />}
      </button>
    </div>
  );
}

export function Composer() {
  const session = useStore((s) => s.session);
  const panelOpen = useStore((s) => s.panel !== null || s.overlay !== null);
  const [text, setText] = useState("");
  const [menuSel, setMenuSel] = useState(0);
  const [menuClosed, setMenuClosed] = useState(false);
  const ta = useRef<HTMLTextAreaElement>(null);
  const lastEsc = useRef(0);

  const insert = (t: string) => {
    const el = ta.current;
    setText((cur) => {
      if (!el) return cur + t;
      const at = el.selectionStart ?? cur.length;
      const before = cur.slice(0, at);
      const sep = before && !/\s$/.test(before) ? " " : "";
      return before + sep + t + cur.slice(el.selectionEnd ?? at);
    });
    requestAnimationFrame(() => el?.focus());
  };
  const voice = useVoice(insert);

  useEffect(
    () =>
      store.onComposer((t, mode) => {
        if (mode === "replace") setText(t);
        else insert(t);
        requestAnimationFrame(() => ta.current?.focus());
      }),
    [],
  );

  // Keep the box as tall as its text (up to a limit).
  useEffect(() => {
    const el = ta.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 320)}px`;
  }, [text]);

  useEffect(() => {
    if (!panelOpen) ta.current?.focus();
  }, [session?.id, panelOpen]);

  // ctrl+shift+space talks from anywhere on the page.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.code === "Space" && e.ctrlKey && e.shiftKey) {
        e.preventDefault();
        void voice.toggle();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const slash = !menuClosed && /^\/\S*$/.test(text) ? text.slice(1) : null;
  const matches = useMatches(slash ?? "");
  const menuOpen = slash !== null && matches.length > 0;

  const submit = () => {
    const t = text.trim();
    if (!t || !session) return;
    const cmd = /^\/(\S+)/.exec(t)?.[1];
    const known = store.state.commands.find((c) => c.name === `/${cmd}`);
    if (known?.panel) {
      store.openPanel(known.name, t.slice(known.name.length).trim());
    } else {
      store.submit(t);
    }
    setText("");
    setMenuClosed(false);
  };

  const pick = (name: string) => {
    const c = store.state.commands.find((x) => x.name === name);
    setMenuClosed(true);
    if (c && !c.args && !c.panel && c.source === "codeit") {
      setText("");
      store.submit(name);
      setMenuClosed(false);
      return;
    }
    if (c?.panel) {
      setText("");
      setMenuClosed(false);
      store.openPanel(name);
      return;
    }
    setText(`${name} `);
    setMenuClosed(false);
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (menuOpen) {
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        const n = matches.length;
        setMenuSel((s) => (s + (e.key === "ArrowDown" ? 1 : n - 1)) % n);
        return;
      }
      if (e.key === "Tab") {
        e.preventDefault();
        setText(`${matches[Math.min(menuSel, matches.length - 1)].name} `);
        return;
      }
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        pick(matches[Math.min(menuSel, matches.length - 1)].name);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setMenuClosed(true);
        return;
      }
    }
    if (e.key === "Enter" && !e.shiftKey && !e.altKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      submit();
      return;
    }
    if (e.key === "Tab" && !e.shiftKey && session) {
      e.preventDefault();
      const names = store.state.agents.map((a) => a.name);
      const next = names[(names.indexOf(session.settings.agent) + 1) % names.length];
      if (next) store.send({ type: "agent", session: session.id, agent: next });
      return;
    }
    if (e.key === "Escape") {
      if (voice.state.kind === "recording") {
        voice.cancel();
        return;
      }
      if (session?.busy) {
        store.stop();
        return;
      }
      const now = Date.now();
      if (now - lastEsc.current < 600) setText("");
      lastEsc.current = now;
      return;
    }
    if (e.key === "ArrowUp" && !text && session?.queue.length) {
      e.preventDefault();
      store.send({ type: "unqueue", session: session.id });
    }
  };

  const onPaste = async (e: React.ClipboardEvent) => {
    const file = Array.from(e.clipboardData.files).find((f) => f.type.startsWith("image/"));
    if (!file) return;
    e.preventDefault();
    await attach(file);
  };

  const attach = async (file: File) => {
    const res = await fetch("/api/image", { method: "POST", headers: { "Content-Type": file.type }, body: file });
    if (!res.ok) {
      store.toast(await res.text(), true);
      return;
    }
    const { path } = (await res.json()) as { path: string };
    insert(`@${path} `);
  };

  if (!session) return null;
  return (
    <div className="composerwrap">
      <div className="column">
        <Asks />
        {session.queue.length > 0 && (
          <div className="queue">
            {session.queue.map((q, i) => (
              <div key={i} className="queued">
                <span className="muted">Queued</span> {q}
              </div>
            ))}
            <span className="muted small">↑ takes the last one back</span>
          </div>
        )}
        <ContextNotice />
        {voice.partial && <div className="heard">{voice.partial}</div>}
        {menuOpen && <CommandMenu query={slash ?? ""} selected={Math.min(menuSel, matches.length - 1)} onPick={pick} />}
        <div
          className={`composer${voice.state.kind === "recording" ? " recording" : ""}`}
          onDragOver={(e) => e.preventDefault()}
          onDrop={(e) => {
            const f = Array.from(e.dataTransfer.files).find((x) => x.type.startsWith("image/"));
            if (f) {
              e.preventDefault();
              void attach(f);
            }
          }}
        >
          <textarea
            ref={ta}
            rows={1}
            value={text}
            placeholder={
              voice.state.kind === "recording"
                ? "Listening… click the mic (or ctrl+shift+space) when you're done"
                : voice.state.kind === "working"
                  ? "Transcribing…"
                  : session.busy
                    ? "Send a message to queue it…"
                    : "Ask codeit to do something… (/ for commands, @path to attach a file)"
            }
            onChange={(e) => {
              setText(e.target.value);
              setMenuSel(0);
              if (!e.target.value.startsWith("/")) setMenuClosed(false);
            }}
            onKeyDown={onKeyDown}
            onPaste={(e) => void onPaste(e)}
          />
          <div className="composerrow">
            <Toolbar voice={voice} />
            {session.busy && !text.trim() ? (
              <button className="sendbtn stop" onClick={() => store.stop()} title="Stop (esc)">
                <StopIcon />
              </button>
            ) : (
              <button className="sendbtn" disabled={!text.trim()} onClick={submit} title="Send (enter)">
                <SendIcon />
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
