import { memo, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { Item, Tool } from "../protocol";
import { store, useStore } from "../store";
import { duration, failed as didFail, result, verb } from "../format";
import { CopyButton, Markdown } from "./Markdown";

export function Chevron({ open }: { open: boolean }) {
  return (
    <svg className="chev" viewBox="0 0 10 10" width="10" height="10" aria-hidden>
      <path d={open ? "M2 3.5l3 3 3-3" : "M3.5 2l3 3-3 3"} fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

function Spinner() {
  return <span className="spinner" aria-label="working" />;
}

function Diff({ diff }: { diff: string }) {
  return (
    <pre className="diff">
      {diff.replace(/\n$/, "").split("\n").map((l, i) => {
        const cls = l.startsWith("+++") || l.startsWith("---")
          ? "meta"
          : l.startsWith("+")
            ? "add"
            : l.startsWith("-")
              ? "del"
              : l.startsWith("@@")
                ? "hunk"
                : "";
        return (
          <div key={i} className={cls}>
            {l || " "}
          </div>
        );
      })}
    </pre>
  );
}

/** What a call's line says: a command by what it does (its description), else the verb and
 * the call's title. */
function labelOf(t: Tool): { text: string; verb: string | null } {
  const d = t.input?.description;
  if (t.name === "bash" && typeof d === "string" && d.trim()) return { text: d.trim(), verb: null };
  return { text: t.title, verb: verb(t.name) };
}

/** A command the call ran, a pattern it searched for: what the line leaves out. */
function detailOf(t: Tool, shown: string): string | null {
  const input = t.input ?? {};
  for (const k of ["command", "pattern", "url", "prompt", "description"]) {
    const v = input[k];
    if (typeof v === "string" && v.trim() && v !== shown && v !== t.title) return v;
  }
  return t.name === "bash" && t.title !== shown ? t.title : null;
}

/** One line per call, as a step in the conversation; click it for what it ran and got. */
const ToolRow = memo(function ToolRow({ t }: { t: Tool }) {
  const [open, setOpen] = useState(false);
  const running = t.state === "running" || t.state === "pending";
  const failed = didFail(t);
  const res = result(t);
  const label = labelOf(t);
  const detail = detailOf(t, label.text);
  const body = t.diff || t.output || t.live || detail;
  return (
    <div className={`tool ${failed ? "failed" : t.state}${open ? " open" : ""}`}>
      <button className="toolhead" onClick={() => body && setOpen(!open)} disabled={!body}>
        <span className="toolicon">{running ? <Spinner /> : <span className="tooldot" />}</span>
        {label.verb && <span className="toolverb">{label.verb}</span>}
        <span className={label.verb ? "tooltitle" : "tooldesc"} title={detail ?? t.title}>
          {label.text || (t.state === "pending" ? "…" : "")}
        </span>
        <span className={`toolres${failed ? " bad" : ""}`}>{res}</span>
        {t.secs != null && t.secs >= 0.5 && <span className="tooltime">{duration(t.secs)}</span>}
        {body && <Chevron open={open} />}
      </button>
      {running && t.live && !open && <pre className="live">{t.live.split("\n").slice(-4).join("\n")}</pre>}
      {open && (
        <div className="toolbody">
          {detail && (
            <div className="tooldetail">
              <pre>{detail}</pre>
              <CopyButton text={detail} />
            </div>
          )}
          {t.diff ? <Diff diff={t.diff} /> : null}
          {!t.diff && (t.output || t.live) && <pre className="output">{t.output || t.live}</pre>}
          {t.digest && (
            <div className="digest">
              <b>What the model got (condensed):</b> {t.digest}
            </div>
          )}
        </div>
      )}
    </div>
  );
});

const Thinking = memo(function Thinking({ text, live }: { text: string; live: boolean }) {
  const [open, setOpen] = useState(false);
  const shown = live || open;
  return (
    <div className={`thinking${shown ? " open" : ""}`}>
      <button onClick={() => setOpen(!open)} className="thinkhead">
        {live ? <Spinner /> : <Chevron open={open} />} Thinking
      </button>
      {shown && <div className="thinkbody">{live ? text.slice(-1500) : text}</div>}
    </div>
  );
});

/** Calls in a row are one block, so the conversation reads as text with its actions between. */
function Steps({ tools }: { tools: Tool[] }) {
  return (
    <div className="steps">
      {tools.map((t) => (
        <ToolRow key={t.id} t={t} />
      ))}
    </div>
  );
}

function group(items: Item[]): (Item | Tool[])[] {
  const out: (Item | Tool[])[] = [];
  for (const it of items) {
    const last = out.at(-1);
    if (it.kind === "tool") {
      if (Array.isArray(last)) last.push(it);
      else out.push([it]);
    } else out.push(it);
  }
  return out;
}

function Entry({ item, last, busy }: { item: Item; last: boolean; busy: boolean }) {
  switch (item.kind) {
    case "user":
      return (
        <div className="user">
          <div className="bubble">{item.text}</div>
        </div>
      );
    case "assistant": {
      const streaming = !item.done && busy && last;
      return (
        <div className="assistant">
          {item.reasoning && <Thinking text={item.reasoning} live={streaming && !item.text} />}
          {item.text && <Markdown text={item.text} />}
          {item.done && item.text && (
            <div className="answertools">
              <CopyButton text={item.text} />
            </div>
          )}
        </div>
      );
    }
    case "notice":
      return <div className={`notice${item.error ? " error" : ""}`}>{item.text}</div>;
    case "turn_end":
      return (
        <div className="turnend">
          {[item.agent, item.model, item.effort, item.secs != null ? duration(item.secs) : null]
            .filter(Boolean)
            .join(" · ")}
        </div>
      );
    default:
      return null;
  }
}

function Working() {
  const started = useStore((s) => s.session?.started ?? null);
  const items = useStore((s) => s.session?.items);
  const [, tick] = useState(0);
  useEffect(() => {
    const t = setInterval(() => tick((n) => n + 1), 1000);
    return () => clearInterval(t);
  }, []);
  const running = items?.findLast((i): i is Tool => i.kind === "tool" && i.state === "running");
  const secs = started ? (Date.now() - started) / 1000 : 0;
  return (
    <div className="working">
      <Spinner />
      <span>{running ? `${verb(running.name)} ${running.title}` : "Working"}</span>
      <span className="muted">
        {duration(secs)} · <button className="link" onClick={() => store.stop()}>esc to stop</button>
      </span>
    </div>
  );
}

function Empty() {
  const project = useStore((s) => s.project);
  const commands = useStore((s) => s.commands);
  const panels = commands.filter((c) => c.panel);
  return (
    <div className="empty">
      <div className="emptylogo">{"</>"}</div>
      <h1>What should we build{project ? ` in ${project}` : ""}?</h1>
      <p className="muted">
        Describe a task, or type <kbd>/</kbd> for commands. <kbd>Tab</kbd> switches between build and plan.
      </p>
      {panels.length > 0 && (
        <div className="starters">
          {panels.slice(0, 6).map((c) => (
            <button key={c.name} className="starter" onClick={() => store.openPanel(c.name)}>
              <span className="pluginbadge">{c.source}</span>
              <b>{c.name}</b>
              <span className="muted">{c.help}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

export function Conversation() {
  const session = useStore((s) => s.session);
  const scroller = useRef<HTMLDivElement>(null);
  const stick = useRef(true);

  // Follows the end while you are at the end; leaves you where you are after you scroll up.
  useLayoutEffect(() => {
    const el = scroller.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  });
  // A new session, or a message you sent: back to the end.
  const prompts = session?.items.filter((i) => i.kind === "user").length ?? 0;
  useEffect(() => {
    stick.current = true;
    const el = scroller.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [session?.id, prompts]);

  if (!session) return <div className="conversation" />;
  const groups = group(session.items);
  return (
    <div
      className="conversation"
      ref={scroller}
      onScroll={(e) => {
        const el = e.currentTarget;
        stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
      }}
    >
      <div className="column">
        {session.items.length === 0 && !session.busy && <Empty />}
        {groups.map((g, i) =>
          Array.isArray(g) ? (
            <Steps key={g[0].id} tools={g} />
          ) : (
            <Entry key={i} item={g} last={i === groups.length - 1} busy={session.busy} />
          ),
        )}
        {session.busy && <Working />}
      </div>
    </div>
  );
}
