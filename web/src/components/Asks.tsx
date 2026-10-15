import { useEffect, useState } from "react";
import type { PermissionAsk, QuestionAsk } from "../protocol";
import { store, useStore } from "../store";

function Permission({ id, ask }: { id: number; ask: PermissionAsk }) {
  const [denying, setDenying] = useState(false);
  const [why, setWhy] = useState("");
  const answer = (reply: "once" | "always" | "reject", message?: string) =>
    store.send({ type: "permission", id, reply, message });
  const always = ask.always.includes("*") ? "everything" : ask.always.join(", ");

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // Keys answer unless you are typing something (an empty message box counts as not typing).
      const t = e.target as HTMLElement | null;
      const typing = (t instanceof HTMLTextAreaElement || t instanceof HTMLInputElement) && t.value !== "";
      if (denying || typing || e.metaKey || e.ctrlKey || e.altKey) return;
      if (e.key === "y") answer("once");
      else if (e.key === "a") answer("always");
      else if (e.key === "n") setDenying(true);
      else return;
      e.preventDefault();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  return (
    <div className="ask">
      <div className="askhead">
        <span className="askicon">?</span>
        <span>
          Allow <b>{ask.title}</b>?
        </span>
      </div>
      {ask.detail && <pre className="askdetail">{ask.detail}</pre>}
      {denying ? (
        <form
          className="askrow"
          onSubmit={(e) => {
            e.preventDefault();
            answer("reject", why);
          }}
        >
          <input autoFocus placeholder="What should it do instead? (optional)" value={why} onChange={(e) => setWhy(e.target.value)} />
          <button type="submit" className="btn danger">Deny</button>
          <button type="button" className="btn" onClick={() => setDenying(false)}>Back</button>
        </form>
      ) : (
        <div className="askrow">
          <button className="btn primary" onClick={() => answer("once")}>Yes <kbd>y</kbd></button>
          <button className="btn" onClick={() => answer("always")} title={`For the rest of this session: ${always}`}>
            Always in this session <kbd>a</kbd>
          </button>
          <button className="btn" onClick={() => setDenying(true)}>No <kbd>n</kbd></button>
        </div>
      )}
    </div>
  );
}

function Questions({ id, ask }: { id: number; ask: QuestionAsk }) {
  const [picked, setPicked] = useState<string[][]>(ask.questions.map(() => []));
  const [other, setOther] = useState<string[]>(ask.questions.map(() => ""));
  const toggle = (q: number, label: string, multiple: boolean) =>
    setPicked((p) =>
      p.map((sel, i) =>
        i !== q ? sel : multiple ? (sel.includes(label) ? sel.filter((x) => x !== label) : [...sel, label]) : [label],
      ),
    );
  const answers = ask.questions.map((_, i) => [...picked[i], ...(other[i].trim() ? [other[i].trim()] : [])]);
  const ready = answers.every((a) => a.length > 0);
  return (
    <div className="ask">
      {ask.questions.map((q, i) => (
        <div key={i} className="question">
          {q.header && <div className="qheader">{q.header}</div>}
          <div className="qtext">{q.question}</div>
          <div className="options">
            {q.options.map((o) => (
              <button
                key={o.label}
                className={`option${picked[i].includes(o.label) ? " on" : ""}`}
                onClick={() => toggle(i, o.label, q.multiple)}
              >
                <b>{o.label}</b>
                {o.description && <span className="muted">{o.description}</span>}
              </button>
            ))}
            <input
              placeholder="Or type your own answer"
              value={other[i]}
              onChange={(e) => setOther((o) => o.map((x, j) => (j === i ? e.target.value : x)))}
            />
          </div>
        </div>
      ))}
      <div className="askrow">
        <button className="btn primary" disabled={!ready} onClick={() => store.send({ type: "question", id, answers })}>
          Answer
        </button>
        <button className="btn" onClick={() => store.send({ type: "question", id, answers: null })}>
          Dismiss
        </button>
      </div>
    </div>
  );
}

export function Asks() {
  const asks = useStore((s) => s.session?.asks);
  const first = asks?.[0];
  if (!first) return null;
  return first.ask.kind === "permission" ? (
    <Permission key={first.id} id={first.id} ask={first.ask} />
  ) : (
    <Questions key={first.id} id={first.id} ask={first.ask} />
  );
}
