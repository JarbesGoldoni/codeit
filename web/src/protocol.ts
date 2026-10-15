// What the server sends and takes (crates/codeit/src/web/mod.rs).

export type Tone = "normal" | "ok" | "warn" | "bad" | "muted" | "accent";

export interface Tool {
  kind: "tool";
  id: string;
  name: string;
  title: string;
  state: "pending" | "running" | "done" | "failed";
  output: string;
  diff: string | null;
  lines: number | null;
  digest: string | null;
  live: string;
  progress: string[];
  input: Record<string, unknown> | null;
  secs: number | null;
}

export type Item =
  | { kind: "user"; text: string }
  | { kind: "assistant"; text: string; reasoning: string; done: boolean }
  | Tool
  | { kind: "notice"; text: string; error: boolean }
  | { kind: "turn_end"; agent: string; model: string; effort: string | null; secs: number | null };

export type Op =
  | { op: "push"; item: Item }
  | { op: "append"; index: number; field: "text" | "reasoning" | "live"; text: string }
  | { op: "set"; index: number; item: Item }
  | { op: "truncate"; len: number };

export interface Model {
  key: string;
  provider: string;
  id: string;
  name: string;
  context: number | null;
  efforts: string[];
  default_effort: string | null;
  vision: boolean;
}

export interface Models {
  list: Model[];
  loading: number;
  failed: { provider: string; error: string }[];
}

export interface Command {
  name: string;
  args: string;
  help: string;
  long: string;
  /// "codeit", "prompt" (a prompt command from the config), or the plugin's name.
  source: string;
  panel: boolean;
}

export interface Plugin {
  name: string;
  commands: string[];
}

export interface Settings {
  model: string | null;
  effort: string | null;
  agent: string;
  approval: "auto" | "ask";
}

export interface Todo {
  content: string;
  status: "pending" | "in_progress" | "completed" | "cancelled";
}

export interface SessionMeta {
  id: string;
  title: string;
  updated: number;
  busy: boolean;
  waiting: boolean;
}

export interface PermissionAsk {
  kind: "permission";
  permission: string;
  patterns: string[];
  always: string[];
  title: string;
  detail: string | null;
}

export interface Question {
  question: string;
  header: string;
  options: { label: string; description: string }[];
  multiple: boolean;
}

export interface QuestionAsk {
  kind: "question";
  questions: Question[];
}

export type AskView = PermissionAsk | QuestionAsk;

export interface PanelRow {
  id: string | null;
  label: string;
  value: string;
  tone: Tone;
  detail: string[];
  bar: number | null;
}

export interface Panel {
  title: string;
  sections: { title: string; rows: PanelRow[] }[];
  message: { text: string; tone: Tone } | null;
  keys: { key: string; label: string }[];
  poll_ms: number | null;
  select: string | null;
}

export type PanelReply =
  | { kind: "panel"; panel: Panel }
  | { kind: "input"; title: string; hint: string; secret: boolean }
  | { kind: "notice"; text: string }
  | { kind: "close" };

export interface PluginStatus {
  plugin: string;
  pieces: { text: string; tone: Tone }[];
}

export interface Git {
  branch: string;
  added: number;
  changed: number;
}

export interface VoiceModel {
  name: string;
  mb: number;
  what: string;
  ready: boolean;
}

export interface Voice {
  engine: string | null;
  model: string;
  language: string;
  custom_model: boolean;
  models: VoiceModel[];
  downloading: { model: string; done: number; total: number | null } | null;
  error: string | null;
}

export interface BridgeSite {
  key: string;
  plugin: string;
  title: string;
  url: string;
  cookies: string[];
  keepalive: number | null;
  ok: boolean | null;
  detail: string | null;
  at: number | null;
}

export interface Bridge {
  browsers: string[];
  sites: BridgeSite[];
}

export interface LoginChoice {
  id: string;
  name: string;
  hint: string;
  logged_in: boolean;
  methods: { id: string; label: string }[];
  doc: string | null;
  env: string[];
  how: string;
}

export type ServerMessage =
  | {
      type: "hello";
      version: string;
      cwd: string;
      project: string;
      agents: { name: string; description: string }[];
      commands: Command[];
      plugins: Plugin[];
      models: Models;
      settings: { model: string | null; effort: string | null; approval: "auto" | "ask" };
      voice: Voice;
      status: PluginStatus[];
      git: Git | null;
      bridge: Bridge;
      problems: string[];
      sessions: SessionMeta[];
    }
  | {
      type: "history";
      session: string;
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
      /** Sent to every tab (after /undo): only the tabs showing that session take it. */
      refresh?: boolean;
    }
  | { type: "ops"; session: string; version: number; ops: Op[] }
  | { type: "turn"; session: string; busy: boolean; started: number | null; queue: string[] }
  | { type: "settings"; session: string; settings: Settings }
  | { type: "ask"; session: string; id: number; ask: AskView }
  | { type: "ask_done"; id: number }
  | { type: "todos"; session: string; todos: Todo[] }
  | { type: "title"; session: string; title: string }
  | { type: "context"; session: string; context: [number, number] | null }
  | { type: "sessions"; sessions: SessionMeta[] }
  | { type: "commands"; commands: Command[] }
  | { type: "models"; models: Models }
  | { type: "status"; status: PluginStatus[] }
  | { type: "git"; git: Git | null }
  | { type: "voice"; voice: Voice }
  | { type: "bridge"; bridge: Bridge }
  | { type: "panel"; seq: number; reply: PanelReply }
  | { type: "open_panel"; command: string; args: string }
  | { type: "open"; what: string; args: string }
  | { type: "input"; session: string; text: string }
  | { type: "logins"; providers: LoginChoice[] }
  | { type: "login_started"; provider: string; url: string; code: string }
  | { type: "login_done"; ok: boolean; text: string }
  | { type: "toast"; text: string; error: boolean }
  | { type: "resync" };

export type PanelAction =
  | { kind: "open"; args: string }
  | { kind: "enter"; id: string }
  | { kind: "key"; key: string; id: string | null }
  | { kind: "text"; text: string }
  | { kind: "poll" };

export type ClientMessage =
  | { type: "open"; session: string | null }
  | { type: "send"; session: string; text: string }
  | { type: "stop"; session: string }
  | { type: "unqueue"; session: string }
  | { type: "model"; session: string; model: string }
  | { type: "effort"; session: string; effort: string | null }
  | { type: "agent"; session: string; agent: string }
  | { type: "approval"; session: string; approval: "auto" | "ask" }
  | { type: "permission"; id: number; reply: "once" | "always" | "reject"; message?: string }
  | { type: "question"; id: number; answers: string[][] | null }
  | { type: "panel"; seq: number; session: string; command: string; action: PanelAction }
  | { type: "sessions" }
  | { type: "rename"; session: string; title: string }
  | { type: "delete"; session: string }
  | { type: "logins" }
  | { type: "login_key"; provider: string; key: string }
  | { type: "login_start"; provider: string; method: string; enterprise?: string }
  | { type: "voice"; model?: string; language?: string }
  | { type: "voice_download"; model: string }
  | { type: "bridge_refresh"; site: string }
  | { type: "bridge_reload" };
