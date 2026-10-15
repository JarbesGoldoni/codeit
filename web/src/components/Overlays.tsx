import { useState } from "react";
import { modelOf, store, useStore } from "../store";
import { ago, tokens } from "../format";
import { Palette, type Option } from "./Palette";
import type { LoginChoice } from "../protocol";

function Models() {
  const models = useStore((s) => s.models);
  const session = useStore((s) => s.session);
  const close = () => store.setOverlay(null);
  const current = session?.settings.model;
  const options: Option[] = models.list.map((m) => ({
    id: m.key,
    label: m.name,
    group: m.provider,
    detail: [m.context ? `${tokens(m.context)} context` : "", m.efforts.length ? `effort: ${m.efforts.join("/")}` : "", m.vision ? "images" : ""]
      .filter(Boolean)
      .join(" · "),
    current: m.key === current,
  }));
  return (
    <Palette
      title="Model"
      options={options}
      placeholder="Search models"
      empty={models.loading ? "Loading models…" : "No models: log in to a provider (Logins, bottom left)."}
      onClose={close}
      onPick={(o) => {
        if (session) store.send({ type: "model", session: session.id, model: o.id });
        close();
      }}
      footer={
        <>
          {models.loading > 0 && <span className="muted">Still asking {models.loading} providers… </span>}
          {models.failed.map((f) => (
            <div key={f.provider} className="tone-bad small">
              {f.provider}: {f.error}
            </div>
          ))}
        </>
      }
    />
  );
}

function Effort() {
  const model = useStore(modelOf);
  const session = useStore((s) => s.session);
  const close = () => store.setOverlay(null);
  if (!model || !session) return null;
  const options: Option[] = [
    { id: "", label: "default", detail: model.default_effort ? `the model's own (${model.default_effort})` : "the model's own", current: !session.settings.effort },
    ...model.efforts.map((e) => ({ id: e, label: e, current: session.settings.effort === e })),
  ];
  return (
    <Palette
      title={`Reasoning effort · ${model.name}`}
      options={options}
      onClose={close}
      onPick={(o) => {
        store.send({ type: "effort", session: session.id, effort: o.id || null });
        close();
      }}
    />
  );
}

function Sessions() {
  const sessions = useStore((s) => s.sessions);
  const current = useStore((s) => s.session?.id);
  const close = () => store.setOverlay(null);
  const options: Option[] = sessions.map((s) => ({
    id: s.id,
    label: s.title || "Untitled",
    detail: s.waiting ? "waiting for you" : s.busy ? "working…" : ago(s.updated),
    current: s.id === current,
  }));
  return (
    <Palette
      title="Sessions"
      options={options}
      placeholder="Search sessions"
      empty="No saved sessions in this folder yet."
      onClose={close}
      onPick={(o) => {
        store.open(o.id);
        close();
      }}
    />
  );
}

function Login({ filter }: { filter: string }) {
  const login = useStore((s) => s.login);
  const [picked, setPicked] = useState<LoginChoice | null>(null);
  const [method, setMethod] = useState<string | null>(null);
  const [key, setKey] = useState("");
  const [domain, setDomain] = useState("");
  const close = () => store.setOverlay(null);

  if (!picked) {
    const options: Option[] = (login.providers ?? [])
      .filter((p) => !filter || p.id === filter || p.name.toLowerCase().includes(filter.toLowerCase()))
      .map((p) => ({ id: p.id, label: p.name, detail: p.hint, current: p.logged_in }));
    return (
      <Palette
        title="Log in to a provider"
        options={options}
        placeholder="Search providers (✓ logged in)"
        empty={login.providers ? "No provider matches." : "Loading…"}
        onClose={close}
        onPick={(o) => {
          const p = login.providers?.find((x) => x.id === o.id) ?? null;
          setPicked(p);
          if (p && p.methods.length === 1) setMethod(p.methods[0].id);
        }}
      />
    );
  }
  if (!method) {
    return (
      <Palette
        title={`Log in to ${picked.name}`}
        options={picked.methods.map((m) => ({ id: m.id, label: m.label }))}
        onClose={close}
        onPick={(o) => setMethod(o.id)}
      />
    );
  }
  const body = () => {
    if (login.done) {
      return (
        <>
          <p className={login.done.ok ? "tone-ok" : "tone-bad"}>{login.done.text}</p>
          <button className="btn primary" onClick={close}>Done</button>
        </>
      );
    }
    if (method === "ApiKey") {
      return (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            store.send({ type: "login_key", provider: picked.id, key });
          }}
        >
          {picked.doc && (
            <p className="muted">
              Get a key at <a href={picked.doc} target="_blank" rel="noreferrer">{picked.doc}</a>
            </p>
          )}
          {picked.env.length > 0 && <p className="muted small">Setting {picked.env.join(" or ")} works too.</p>}
          <input type="password" autoFocus placeholder="API key" value={key} onChange={(e) => setKey(e.target.value)} />
          <div className="row">
            <button className="btn primary" disabled={!key.trim()}>Save</button>
            <button type="button" className="btn" onClick={close}>Cancel</button>
          </div>
        </form>
      );
    }
    if (method === "Own") {
      return (
        <>
          <p>{picked.how}</p>
          <button className="btn" onClick={close}>Close</button>
        </>
      );
    }
    if (login.started) {
      return (
        <>
          <p>
            Open <a href={login.started.url} target="_blank" rel="noreferrer">{login.started.url}</a>
            {login.started.code ? " and enter the code:" : " (it opened in your browser)."}
          </p>
          {login.started.code && <div className="devicecode">{login.started.code}</div>}
          <p className="muted"><span className="spinner small" /> Waiting for you to approve…</p>
        </>
      );
    }
    if (method === "CopilotEnterprise" && !domain.includes(".")) {
      return (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            store.send({ type: "login_start", provider: picked.id, method, enterprise: domain });
          }}
        >
          <input autoFocus placeholder="company.ghe.com" value={domain} onChange={(e) => setDomain(e.target.value)} />
          <button className="btn primary">Continue</button>
        </form>
      );
    }
    return (
      <>
        <p className="muted">Starts a login in your browser.</p>
        <button
          className="btn primary"
          onClick={() => store.send({ type: "login_start", provider: picked.id, method, enterprise: domain || undefined })}
        >
          Start
        </button>
      </>
    );
  };
  return (
    <div className="overlay" onMouseDown={close}>
      <div className="dialog" onMouseDown={(e) => e.stopPropagation()} onKeyDown={(e) => e.key === "Escape" && close()}>
        <div className="dialoghead">Log in to {picked.name}</div>
        <div className="dialogbody">{body()}</div>
      </div>
    </div>
  );
}

const LANGUAGES = [
  ["auto", "Detect"],
  ["en", "English"],
  ["pt", "Portuguese"],
  ["es", "Spanish"],
  ["fr", "French"],
  ["de", "German"],
  ["it", "Italian"],
];

function VoiceSettings() {
  const voice = useStore((s) => s.voice);
  const close = () => store.setOverlay(null);
  if (!voice) return null;
  const english = voice.model.endsWith(".en");
  return (
    <div className="overlay" onMouseDown={close}>
      <div className="dialog wide" onMouseDown={(e) => e.stopPropagation()} onKeyDown={(e) => e.key === "Escape" && close()}>
        <div className="dialoghead">Voice</div>
        <div className="dialogbody">
          {voice.engine ? (
            <p className="muted">
              Click the mic in the message box, or press <kbd>ctrl+shift+space</kbd>, talk, then press it again. Whisper turns
              it into text on this machine ({voice.engine}); you check it before sending. Nothing leaves your computer.
            </p>
          ) : (
            <div className="setup">
              <p>
                <b>Speech to text isn't set up in this build.</b> Either:
              </p>
              <ul>
                <li>
                  build codeit with whisper.cpp inside: <code>cargo install … --features voice</code> (needs <code>cmake</code> and a C++
                  compiler: <code>sudo apt install cmake build-essential</code> on Ubuntu/WSL), or
                </li>
                <li>
                  put whisper.cpp's <code>whisper-cli</code> on your PATH and restart <code>codeit web</code>.
                </li>
              </ul>
            </div>
          )}
          <h4>Model</h4>
          {voice.custom_model && <p className="muted small">CODEIT_WHISPER_MODEL is set: that file is used whatever you pick.</p>}
          <div className="models">
            {voice.models.map((m) => {
              const dl = voice.downloading?.model === m.name ? voice.downloading : null;
              return (
                <label key={m.name} className={`vmodel${voice.model === m.name ? " on" : ""}`}>
                  <input
                    type="radio"
                    name="vmodel"
                    checked={voice.model === m.name}
                    onChange={() => store.send({ type: "voice", model: m.name })}
                  />
                  <span className="vname">{m.name}</span>
                  <span className="muted">{m.what}</span>
                  <span className="vsize">
                    {dl ? (
                      `${Math.round((dl.done / (dl.total ?? m.mb * 1e6)) * 100)}%`
                    ) : m.ready ? (
                      <span className="tone-ok">downloaded</span>
                    ) : (
                      <button
                        className="link"
                        onClick={(e) => {
                          e.preventDefault();
                          store.send({ type: "voice_download", model: m.name });
                        }}
                      >
                        get ({m.mb} MB)
                      </button>
                    )}
                  </span>
                </label>
              );
            })}
          </div>
          {voice.error && <p className="tone-bad small">{voice.error}</p>}
          <h4>Language</h4>
          <select
            value={english ? "en" : voice.language}
            disabled={english}
            onChange={(e) => store.send({ type: "voice", language: e.target.value })}
          >
            {LANGUAGES.map(([code, name]) => (
              <option key={code} value={code}>
                {name}
              </option>
            ))}
          </select>
          {english && <span className="muted small"> (the .en models only hear English; pick one without .en for other languages)</span>}
        </div>
      </div>
    </div>
  );
}

function BrowserBridge() {
  const bridge = useStore((s) => s.bridge);
  const close = () => store.setOverlay(null);
  return (
    <div className="overlay" onMouseDown={close}>
      <div className="dialog wide" onMouseDown={(e) => e.stopPropagation()} onKeyDown={(e) => e.key === "Escape" && close()}>
        <div className="dialoghead">Browser logins</div>
        <div className="dialogbody">
          <p className="muted">
            codeit's browser extension sends the cookies plugins ask for, as they change, and loads each site now and then so
            its login doesn't expire. Log in once in the morning; codeit stays connected.
          </p>
          <div className="kv">
            <span>Extension</span>
            <span>
              {bridge.browsers.length ? (
                <span className="tone-ok">connected ({bridge.browsers.join(", ")})</span>
              ) : (
                <span className="tone-warn">not connected</span>
              )}
            </span>
          </div>
          {bridge.browsers.length === 0 && (
            <div className="setup">
              <p>To install it (Chrome or Edge, on the computer whose browser you use):</p>
              <ol>
                <li>
                  Open <code>chrome://extensions</code> (or <code>edge://extensions</code>) and turn on <b>Developer mode</b>.
                </li>
                <li>
                  <b>Load unpacked</b> and pick the <code>browser-extension</code> folder of the codeit repo. On WSL, copy it to
                  Windows first (for example <code>cp -r browser-extension /mnt/c/Users/you/codeit-extension</code>).
                </li>
                <li>It connects to codeit on port {location.port || 80} by itself (its options page changes the port).</li>
              </ol>
            </div>
          )}
          <h4>
            Sites plugins ask for
            <button className="link small" onClick={() => store.send({ type: "bridge_reload" })}>
              reload list
            </button>
          </h4>
          {bridge.sites.length === 0 && <p className="muted">No plugin asks for browser cookies.</p>}
          {bridge.sites.map((s) => (
            <div key={s.key} className="siterow">
              <span className={`dot ${s.ok === true ? "ok" : s.ok === false ? "bad" : "off"}`} />
              <div className="grow">
                <div>
                  <b>{s.title}</b> <span className="pluginbadge">{s.plugin}</span>
                </div>
                <div className="muted small ellipsis">{s.url}</div>
                <div className="small">
                  {s.detail ?? "no cookies yet"}
                  {s.at ? <span className="muted"> · {ago(s.at / 1000)}</span> : null}
                  {s.keepalive ? <span className="muted"> · kept alive every {s.keepalive} min</span> : null}
                </div>
              </div>
              <button className="btn small" disabled={!bridge.browsers.length} onClick={() => store.send({ type: "bridge_refresh", site: s.key })}>
                Reload site
              </button>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}

export function Overlays() {
  const overlay = useStore((s) => s.overlay);
  switch (overlay?.kind) {
    case "models":
      return <Models />;
    case "effort":
      return <Effort />;
    case "sessions":
      return <Sessions />;
    case "login":
      return <Login filter={overlay.filter} />;
    case "voice":
      return <VoiceSettings />;
    case "browser":
      return <BrowserBridge />;
    default:
      return null;
  }
}
