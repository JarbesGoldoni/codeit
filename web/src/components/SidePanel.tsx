import { gitOf, store, useStore } from "../store";
import { tokens } from "../format";

const MARK: Record<string, string> = { pending: "○", in_progress: "◐", completed: "●", cancelled: "●" };

export function SidePanel() {
  const session = useStore((s) => s.session);
  const git = useStore(gitOf);
  const status = useStore((s) => s.status);
  const bridge = useStore((s) => s.bridge);
  const cwd = useStore((s) => s.cwd);
  if (!session) return null;
  const ctx = session.context;
  const folder = session.project ? `${cwd}/${session.project}` : cwd;
  return (
    <aside className="sidepanel">
      <section>
        <h3>Session</h3>
        <div className="kv">
          <span>Context</span>
          <span>{ctx ? `${tokens(ctx[0])} / ${tokens(ctx[1])}` : "—"}</span>
        </div>
        {ctx && (
          <div className="meter">
            <span style={{ width: `${Math.min(100, (ctx[0] / ctx[1]) * 100)}%` }} />
          </div>
        )}
        {git && (
          <div className="kv">
            <span>Branch</span>
            <span className="mono">
              {git.branch}
              {git.added > 0 && <span className="ok"> +{git.added}</span>}
              {git.changed > 0 && <span className="warn"> ~{git.changed}</span>}
            </span>
          </div>
        )}
        <div className="kv">
          <span>Folder</span>
          <span className="mono ellipsis" title={folder}>
            {folder}
          </span>
        </div>
      </section>

      {session.todos.length > 0 && (
        <section>
          <h3>
            Todos
            <button className="link small" onClick={() => store.submit("/todos clear")}>
              clear
            </button>
          </h3>
          <ul className="todos">
            {session.todos.map((t, i) => (
              <li key={i} className={t.status}>
                <span className="mark">{MARK[t.status] ?? "○"}</span>
                <span>{t.content}</span>
              </li>
            ))}
          </ul>
        </section>
      )}

      {status.length > 0 && (
        <section>
          <h3>Plugins</h3>
          {status.map((p) => (
            <div key={p.plugin} className="pstatus">
              <span className="pluginbadge">{p.plugin}</span>
              <span>
                {p.pieces.map((x, i) => (
                  <span key={i} className={`tone-${x.tone}`}>
                    {x.text}
                  </span>
                ))}
              </span>
            </div>
          ))}
        </section>
      )}

      {bridge.sites.length > 0 && (
        <section>
          <h3>
            Browser logins
            <button className="link small" onClick={() => store.setOverlay({ kind: "browser" })}>
              details
            </button>
          </h3>
          {bridge.browsers.length === 0 && <div className="muted small">The browser extension isn't connected.</div>}
          {bridge.sites.map((s) => (
            <div key={s.key} className="site">
              <span className={`dot ${s.ok === true ? "ok" : s.ok === false ? "bad" : "off"}`} />
              <span>{s.title}</span>
              <span className="muted small ellipsis">{s.detail ?? "waiting for cookies"}</span>
            </div>
          ))}
        </section>
      )}
    </aside>
  );
}
