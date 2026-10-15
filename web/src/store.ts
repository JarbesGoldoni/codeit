// The app's state, kept in step with the server over one WebSocket.

import { useSyncExternalStore } from "react";
import { applyOps } from "./timeline";
import type {
  AskView,
  Bridge,
  ClientMessage,
  Command,
  Git,
  Item,
  LoginChoice,
  Models,
  Panel,
  PanelAction,
  Plugin,
  PluginStatus,
  ServerMessage,
  SessionMeta,
  Settings,
  Todo,
  Voice,
} from "./protocol";

export interface SessionView {
  id: string;
  version: number;
  title: string;
  saved: boolean;
  items: Item[];
  busy: boolean;
  started: number | null;
  queue: string[];
  asks: { id: number; ask: AskView }[];
  todos: Todo[];
  context: [number, number] | null;
  settings: Settings;
}

export interface PanelState {
  command: string;
  plugin: string;
  seq: number;
  busy: boolean;
  panel: Panel | null;
  input: { title: string; hint: string; secret: boolean } | null;
}

export type Overlay =
  | { kind: "models" }
  | { kind: "effort" }
  | { kind: "sessions" }
  | { kind: "login"; filter: string }
  | { kind: "voice" }
  | { kind: "browser" }
  | null;

export interface Toast {
  id: number;
  text: string;
  error: boolean;
}

export interface LoginState {
  providers: LoginChoice[] | null;
  started: { provider: string; url: string; code: string } | null;
  done: { ok: boolean; text: string } | null;
}

export interface State {
  connected: boolean;
  everConnected: boolean;
  project: string;
  cwd: string;
  version: string;
  agents: { name: string; description: string }[];
  problems: string[];
  commands: Command[];
  plugins: Plugin[];
  models: Models;
  defaults: { model: string | null; effort: string | null; approval: "auto" | "ask" };
  status: PluginStatus[];
  git: Git | null;
  voice: Voice | null;
  bridge: Bridge;
  sessions: SessionMeta[];
  session: SessionView | null;
  panel: PanelState | null;
  overlay: Overlay;
  toasts: Toast[];
  login: LoginState;
}

const initial: State = {
  connected: false,
  everConnected: false,
  project: "",
  cwd: "",
  version: "",
  agents: [],
  problems: [],
  commands: [],
  plugins: [],
  models: { list: [], loading: 0, failed: [] },
  defaults: { model: null, effort: null, approval: "auto" },
  status: [],
  git: null,
  voice: null,
  bridge: { browsers: [], sites: [] },
  sessions: [],
  session: null,
  panel: null,
  overlay: null,
  toasts: [],
  login: { providers: null, started: null, done: null },
};

type Listener = () => void;

/** Text to put in the composer, replacing what is there or inserted at the cursor. */
type ComposerListener = (text: string, mode: "replace" | "insert") => void;

class Store {
  state: State = initial;
  private listeners = new Set<Listener>();
  private composer = new Set<ComposerListener>();
  private ws: WebSocket | null = null;
  private retry = 500;
  private nextSeq = 1;
  private nextToast = 1;

  subscribe = (l: Listener) => {
    this.listeners.add(l);
    return () => {
      this.listeners.delete(l);
    };
  };

  onComposer(l: ComposerListener) {
    this.composer.add(l);
    return () => {
      this.composer.delete(l);
    };
  }

  toComposer(text: string, mode: "replace" | "insert") {
    for (const l of this.composer) l(text, mode);
  }

  private set(patch: Partial<State>) {
    this.state = { ...this.state, ...patch };
    for (const l of this.listeners) l();
  }

  private setSession(patch: Partial<SessionView>) {
    if (!this.state.session) return;
    this.set({ session: { ...this.state.session, ...patch } });
  }

  connect() {
    if (this.ws) return;
    const proto = location.protocol === "https:" ? "wss" : "ws";
    const ws = new WebSocket(`${proto}://${location.host}/api/ws`);
    this.ws = ws;
    ws.onopen = () => {
      this.retry = 500;
      this.set({ connected: true, everConnected: true });
      // After a reconnect, the session on screen; at first, the one in the address.
      const id = this.state.session?.id ?? (location.hash.slice(1) || null);
      this.open(id);
    };
    ws.onmessage = (e) => {
      try {
        this.receive(JSON.parse(e.data) as ServerMessage);
      } catch (err) {
        console.error("bad message from codeit", err);
      }
    };
    ws.onclose = () => {
      this.ws = null;
      this.set({ connected: false });
      setTimeout(() => this.connect(), this.retry);
      this.retry = Math.min(this.retry * 2, 5000);
    };
  }

  send(m: ClientMessage) {
    if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(JSON.stringify(m));
  }

  toast(text: string, error = false) {
    const id = this.nextToast++;
    this.set({ toasts: [...this.state.toasts, { id, text, error }] });
    setTimeout(() => this.set({ toasts: this.state.toasts.filter((t) => t.id !== id) }), error ? 8000 : 4000);
  }

  // ── actions ──────────────────────────────────────────────────────────────

  open(id: string | null) {
    this.send({ type: "open", session: id });
  }

  newSession() {
    this.open(null);
  }

  submit(text: string) {
    const s = this.state.session;
    if (!s || !text.trim()) return;
    this.send({ type: "send", session: s.id, text });
  }

  stop() {
    const s = this.state.session;
    if (s) this.send({ type: "stop", session: s.id });
  }

  setOverlay(overlay: Overlay) {
    if (overlay?.kind === "login") {
      this.set({ login: { providers: null, started: null, done: null } });
      this.send({ type: "logins" });
    }
    this.set({ overlay });
  }

  openPanel(command: string, args = "") {
    const name = command.replace(/^\//, "");
    const plugin = this.state.commands.find((c) => c.name === `/${name}`)?.source ?? "";
    const seq = this.nextSeq++;
    this.set({ panel: { command: name, plugin, seq, busy: true, panel: null, input: null } });
    this.panelSend(seq, name, { kind: "open", args });
  }

  panelAct(action: PanelAction) {
    const p = this.state.panel;
    if (!p) return;
    const seq = this.nextSeq++;
    this.set({ panel: { ...p, seq, busy: action.kind !== "poll" } });
    this.panelSend(seq, p.command, action);
  }

  private panelSend(seq: number, command: string, action: PanelAction) {
    const session = this.state.session?.id ?? "";
    this.send({ type: "panel", seq, session, command, action });
  }

  closePanel() {
    this.set({ panel: null });
  }

  // ── messages ─────────────────────────────────────────────────────────────

  private receive(m: ServerMessage) {
    const s = this.state.session;
    const mine = (id: string) => s?.id === id;
    switch (m.type) {
      case "hello":
        this.set({
          project: m.project,
          cwd: m.cwd,
          version: m.version,
          agents: m.agents,
          problems: m.problems,
          commands: m.commands,
          plugins: m.plugins,
          models: m.models,
          defaults: m.settings,
          voice: m.voice,
          status: m.status,
          git: m.git,
          bridge: m.bridge,
          sessions: m.sessions,
        });
        document.title = m.project ? `${m.project} · codeit` : "codeit";
        break;
      case "history": {
        // Another tab's /undo on a session not shown here.
        if (m.refresh && !mine(m.session)) break;
        this.set({
          session: {
            id: m.session,
            version: m.version,
            title: m.title,
            saved: m.saved,
            items: m.items,
            busy: m.busy,
            started: m.started,
            queue: m.queue,
            asks: m.asks,
            todos: m.todos,
            context: m.context,
            settings: m.settings,
          },
        });
        if (location.hash.slice(1) !== m.session) history.replaceState(null, "", `#${m.session}`);
        break;
      }
      case "ops":
        if (!s || !mine(m.session) || m.version <= s.version) break;
        if (m.version !== s.version + 1) {
          // Missed a change: start over from the server's copy.
          this.open(s.id);
          break;
        }
        this.setSession({ version: m.version, items: applyOps(s.items, m.ops) });
        break;
      case "turn":
        if (mine(m.session)) this.setSession({ busy: m.busy, started: m.started, queue: m.queue });
        break;
      case "settings":
        if (mine(m.session)) this.setSession({ settings: m.settings });
        this.set({
          defaults: { model: m.settings.model, effort: m.settings.effort, approval: m.settings.approval },
        });
        break;
      case "ask":
        if (mine(m.session) && s) this.setSession({ asks: [...s.asks, { id: m.id, ask: m.ask }] });
        break;
      case "ask_done":
        if (s) this.setSession({ asks: s.asks.filter((a) => a.id !== m.id) });
        break;
      case "todos":
        if (mine(m.session)) this.setSession({ todos: m.todos });
        break;
      case "title":
        if (mine(m.session)) this.setSession({ title: m.title, saved: true });
        break;
      case "context":
        if (mine(m.session)) this.setSession({ context: m.context });
        break;
      case "sessions":
        this.set({ sessions: m.sessions });
        if (s && !s.saved && m.sessions.some((x) => x.id === s.id)) this.setSession({ saved: true });
        break;
      case "commands":
        this.set({ commands: m.commands });
        break;
      case "models":
        this.set({ models: m.models });
        break;
      case "status":
        this.set({ status: m.status });
        break;
      case "git":
        this.set({ git: m.git });
        break;
      case "voice":
        this.set({ voice: m.voice });
        break;
      case "bridge":
        this.set({ bridge: m.bridge });
        break;
      case "panel": {
        const p = this.state.panel;
        if (!p || p.seq !== m.seq) break;
        const r = m.reply;
        if (r.kind === "panel") this.set({ panel: { ...p, busy: false, panel: r.panel, input: null } });
        else if (r.kind === "input")
          this.set({ panel: { ...p, busy: false, input: { title: r.title, hint: r.hint, secret: r.secret } } });
        // A notice lands in the conversation (the server adds it there).
        else this.set({ panel: null });
        break;
      }
      case "open_panel":
        this.openPanel(m.command, m.args);
        break;
      case "open":
        if (m.what === "new") this.newSession();
        else if (m.what === "models") this.setOverlay({ kind: "models" });
        else if (m.what === "effort") this.setOverlay({ kind: "effort" });
        else if (m.what === "session") this.setOverlay({ kind: "sessions" });
        else if (m.what === "login") this.setOverlay({ kind: "login", filter: m.args });
        break;
      case "input":
        if (mine(m.session)) this.toComposer(m.text, "replace");
        break;
      case "logins":
        this.set({ login: { ...this.state.login, providers: m.providers } });
        break;
      case "login_started":
        this.set({ login: { ...this.state.login, started: m, done: null } });
        break;
      case "login_done":
        this.set({ login: { ...this.state.login, done: { ok: m.ok, text: m.text } } });
        if (m.ok) this.send({ type: "logins" });
        break;
      case "toast":
        this.toast(m.text, m.error);
        break;
      case "resync":
        if (s) this.open(s.id);
        break;
    }
  }
}

export const store = new Store();

export function useStore<T>(select: (s: State) => T): T {
  return useSyncExternalStore(store.subscribe, () => select(store.state));
}

export function modelOf(state: State): import("./protocol").Model | undefined {
  const key = state.session?.settings.model ?? state.defaults.model;
  return state.models.list.find((m) => m.key === key);
}
