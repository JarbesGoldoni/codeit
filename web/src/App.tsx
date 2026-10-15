import { useEffect, useState } from "react";
import { store, useStore } from "./store";
import { Sidebar } from "./components/Sidebar";
import { Conversation } from "./components/Conversation";
import { Composer } from "./components/Composer";
import { SidePanel } from "./components/SidePanel";
import { Overlays } from "./components/Overlays";
import { PanelDialog } from "./components/PanelDialog";
import { ProjectPicker, Workbench } from "./components/Workbench";
import { Icon } from "./code/Icon";
import { loadSkin } from "./code/skin";

function Header({ side, toggleSide }: { side: boolean; toggleSide: () => void }) {
  const session = useStore((s) => s.session);
  const status = useStore((s) => s.status);
  const view = useStore((s) => s.view);
  const [editing, setEditing] = useState(false);
  const [title, setTitle] = useState("");
  if (!session) return <header className="header" />;
  const save = () => {
    setEditing(false);
    if (title.trim() && title !== session.title) store.send({ type: "rename", session: session.id, title });
  };
  return (
    <header className="header">
      {editing ? (
        <input
          className="titleedit"
          autoFocus
          value={title}
          onChange={(e) => setTitle(e.target.value)}
          onBlur={save}
          onKeyDown={(e) => {
            if (e.key === "Enter") save();
            if (e.key === "Escape") setEditing(false);
          }}
        />
      ) : (
        <button
          className="title"
          title="Rename"
          onClick={() => {
            setTitle(session.title);
            setEditing(true);
          }}
        >
          {session.title || "New session"}
        </button>
      )}
      <ProjectPicker compact={view === "code"} />
      <span className="spacer" />
      {view === "chat" &&
        status.map((p) => (
        <span key={p.plugin} className="hstatus" title={`from the ${p.plugin} plugin`}>
          {p.pieces.map((x, i) => (
            <span key={i} className={`tone-${x.tone}`}>
              {x.text}
            </span>
          ))}
        </span>
        ))}
      <button
        className={`iconbtn${view === "code" ? " on" : ""}`}
        onClick={() => store.setView(view === "code" ? "chat" : "code")}
        title={view === "code" ? "Close the code viewer (ctrl+shift+e)" : "Code: files and changes (ctrl+shift+e)"}
      >
        <Icon id="code" size={17} />
      </button>
      {view === "chat" && (
      <button className={`iconbtn${side ? " on" : ""}`} onClick={toggleSide} title="Side panel">
        <svg viewBox="0 0 24 24" width="17" height="17" fill="none" stroke="currentColor" strokeWidth="2">
          <rect x="3" y="4" width="18" height="16" rx="3" />
          <path d="M15 4v16" />
        </svg>
      </button>
      )}
    </header>
  );
}

function Toasts() {
  const toasts = useStore((s) => s.toasts);
  return (
    <div className="toasts">
      {toasts.map((t) => (
        <div key={t.id} className={`toast${t.error ? " error" : ""}`}>
          {t.text}
        </div>
      ))}
    </div>
  );
}

function Problems() {
  const problems = useStore((s) => s.problems);
  const [shown, setShown] = useState(true);
  if (!problems.length || !shown) return null;
  return (
    <div className="banner warn">
      {problems.map((p, i) => (
        <div key={i}>config: {p}</div>
      ))}
      <button className="link" onClick={() => setShown(false)}>
        dismiss
      </button>
    </div>
  );
}

export function App() {
  const connected = useStore((s) => s.connected);
  const ever = useStore((s) => s.everConnected);
  const view = useStore((s) => s.view);
  const skin = useStore((s) => s.skin);
  const [chatWidth, setChatWidth] = useState(() => {
    try {
      return Number(localStorage.getItem("codeit.chatwidth")) || 460;
    } catch {
      return 460;
    }
  });
  const [side, setSide] = useState(() => {
    try {
      return localStorage.getItem("codeit.side") !== "0";
    } catch {
      return true;
    }
  });
  const toggleSide = () => {
    setSide(!side);
    try {
      localStorage.setItem("codeit.side", side ? "0" : "1");
    } catch {
      /* private window */
    }
  };

  useEffect(() => {
    store.connect();
    const onHash = () => {
      const id = location.hash.slice(1);
      if (id && id !== store.state.session?.id) store.open(id);
    };
    const onKey = (e: KeyboardEvent) => {
      const mod = e.metaKey || e.ctrlKey;
      if (e.key === "Escape" && store.state.overlay) {
        e.preventDefault();
        store.setOverlay(null);
      } else if (mod && e.key.toLowerCase() === "k") {
        e.preventDefault();
        store.setOverlay({ kind: "sessions" });
      } else if (mod && e.shiftKey && e.key.toLowerCase() === "m") {
        e.preventDefault();
        store.setOverlay({ kind: "models" });
      } else if (mod && e.shiftKey && e.key.toLowerCase() === "o") {
        e.preventDefault();
        store.newSession();
      } else if (mod && e.shiftKey && e.key.toLowerCase() === "e") {
        e.preventDefault();
        store.setView(store.state.view === "code" ? "chat" : "code");
      }
    };
    window.addEventListener("hashchange", onHash);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("hashchange", onHash);
      window.removeEventListener("keydown", onKey);
    };
  }, []);

  useEffect(() => {
    loadSkin(skin);
  }, [skin]);

  // Drag the line between the code and the conversation.
  const drag = (e: React.MouseEvent) => {
    e.preventDefault();
    const move = (m: MouseEvent) => {
      const w = Math.min(Math.max(window.innerWidth - m.clientX, 340), window.innerWidth - 480);
      setChatWidth(w);
    };
    const up = (m: MouseEvent) => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      try {
        localStorage.setItem("codeit.chatwidth", String(Math.round(window.innerWidth - m.clientX)));
      } catch {
        /* private window */
      }
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };

  const coding = view === "code";
  return (
    <div
      className={`app${coding ? " coding" : side ? " withside" : ""}`}
      style={coding ? { ["--chat" as string]: `${chatWidth}px` } : undefined}
    >
      {coding ? <Workbench /> : <Sidebar />}
      {coding && <div className="splitter" onMouseDown={drag} />}
      <main className="main">
        <Header side={side} toggleSide={toggleSide} />
        {!connected && ever && <div className="banner bad">Lost the connection to codeit; reconnecting…</div>}
        {!connected && !ever && <div className="banner">Connecting to codeit…</div>}
        <Problems />
        <Conversation />
        <Composer />
      </main>
      {side && !coding && <SidePanel />}
      <Overlays />
      <PanelDialog />
      <Toasts />
    </div>
  );
}
