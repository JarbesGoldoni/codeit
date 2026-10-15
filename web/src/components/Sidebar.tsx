import { useState } from "react";
import { store, useStore } from "../store";
import { ago } from "../format";

export function Sidebar() {
  const project = useStore((s) => s.project);
  const cwd = useStore((s) => s.cwd);
  const sessions = useStore((s) => s.sessions);
  const current = useStore((s) => s.session?.id);
  const currentSaved = useStore((s) => s.session?.saved ?? false);
  const plugins = useStore((s) => s.plugins);
  const commands = useStore((s) => s.commands);
  const connected = useStore((s) => s.connected);
  const voice = useStore((s) => s.voice);
  const bridge = useStore((s) => s.bridge);
  const [filter, setFilter] = useState("");
  const words = filter.toLowerCase().split(/\s+/).filter(Boolean);
  const shown = sessions.filter((s) => words.every((w) => s.title.toLowerCase().includes(w)));

  return (
    <aside className="sidebar">
      <div className="brand">
        <span className="logo">{"</>"}</span>
        <div>
          <div className="brandname">codeit</div>
          <div className="project" title={cwd}>
            {project || cwd}
          </div>
        </div>
        <span className={`dot ${connected ? "ok" : "bad"}`} title={connected ? "Connected" : "Reconnecting…"} />
      </div>

      <button className="newbtn" onClick={() => store.newSession()}>
        <span>＋</span> New session
      </button>

      <div className="sidesection grow">
        <div className="sidetitle">
          Sessions
          {sessions.length > 6 && (
            <input className="sidefilter" placeholder="filter" value={filter} onChange={(e) => setFilter(e.target.value)} />
          )}
        </div>
        <div className="sessionlist">
          {!currentSaved && current && (
            <div className="session on">
              <span className="stitle">New session</span>
            </div>
          )}
          {shown.map((s) => (
            <button key={s.id} className={`session${s.id === current ? " on" : ""}`} onClick={() => store.open(s.id)}>
              <span className="stitle">{s.title || "Untitled"}</span>
              <span className="smeta">
                {s.waiting ? (
                  <span className="badge warn">waiting for you</span>
                ) : s.busy ? (
                  <span className="spinner small" />
                ) : (
                  ago(s.updated)
                )}
              </span>
            </button>
          ))}
          {sessions.length === 0 && <div className="muted small pad">No sessions in this folder yet.</div>}
        </div>
      </div>

      {plugins.length > 0 && (
        <div className="sidesection">
          <div className="sidetitle">Plugins</div>
          {plugins.map((p) => (
            <div key={p.name} className="plugin">
              <div className="pluginname">
                <span className="pluginbadge">{p.name}</span>
              </div>
              <div className="plugincmds">
                {p.commands.map((c) => {
                  const cmd = commands.find((x) => x.name === c);
                  return (
                    <button
                      key={c}
                      className="plugincmd"
                      title={cmd?.help}
                      onClick={() => (cmd?.panel ? store.openPanel(c) : store.toComposer(`${c} `, "replace"))}
                    >
                      {c}
                    </button>
                  );
                })}
              </div>
            </div>
          ))}
        </div>
      )}

      <div className="sidefoot">
        <button className="footbtn" onClick={() => store.setOverlay({ kind: "login", filter: "" })}>
          Logins
        </button>
        <button className="footbtn" onClick={() => store.setOverlay({ kind: "voice" })}>
          Voice <span className={`dot ${voice?.engine ? "ok" : "off"}`} />
        </button>
        <button className="footbtn" onClick={() => store.setOverlay({ kind: "browser" })}>
          Browser <span className={`dot ${bridge.browsers.length ? "ok" : "off"}`} />
        </button>
      </div>
    </aside>
  );
}
