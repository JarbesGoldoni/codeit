// Syntax highlighting with Shiki: VS Code's grammars and themes, so a VS Code color theme (the
// plugin's skin) colors code here the way it does there. Grammars load when first needed.

import { createHighlighterCore, type HighlighterCore, type LanguageRegistration } from "shiki/core";
import { createJavaScriptRegexEngine } from "@shikijs/engine-javascript";

type Loader = () => Promise<{ default: LanguageRegistration[] }>;

const LANGS: Record<string, Loader> = {
  "typescript": () => import("@shikijs/langs/typescript"),
  "tsx": () => import("@shikijs/langs/tsx"),
  "javascript": () => import("@shikijs/langs/javascript"),
  "jsx": () => import("@shikijs/langs/jsx"),
  "json": () => import("@shikijs/langs/json"),
  "jsonc": () => import("@shikijs/langs/jsonc"),
  "json5": () => import("@shikijs/langs/json5"),
  "rust": () => import("@shikijs/langs/rust"),
  "python": () => import("@shikijs/langs/python"),
  "go": () => import("@shikijs/langs/go"),
  "java": () => import("@shikijs/langs/java"),
  "kotlin": () => import("@shikijs/langs/kotlin"),
  "scala": () => import("@shikijs/langs/scala"),
  "groovy": () => import("@shikijs/langs/groovy"),
  "c": () => import("@shikijs/langs/c"),
  "cpp": () => import("@shikijs/langs/cpp"),
  "csharp": () => import("@shikijs/langs/csharp"),
  "swift": () => import("@shikijs/langs/swift"),
  "objective-c": () => import("@shikijs/langs/objective-c"),
  "ruby": () => import("@shikijs/langs/ruby"),
  "php": () => import("@shikijs/langs/php"),
  "lua": () => import("@shikijs/langs/lua"),
  "dart": () => import("@shikijs/langs/dart"),
  "zig": () => import("@shikijs/langs/zig"),
  "shellscript": () => import("@shikijs/langs/shellscript"),
  "fish": () => import("@shikijs/langs/fish"),
  "powershell": () => import("@shikijs/langs/powershell"),
  "bat": () => import("@shikijs/langs/bat"),
  "yaml": () => import("@shikijs/langs/yaml"),
  "toml": () => import("@shikijs/langs/toml"),
  "ini": () => import("@shikijs/langs/ini"),
  "properties": () => import("@shikijs/langs/properties"),
  "dotenv": () => import("@shikijs/langs/dotenv"),
  "markdown": () => import("@shikijs/langs/markdown"),
  "mdx": () => import("@shikijs/langs/mdx"),
  "rst": () => import("@shikijs/langs/rst"),
  "log": () => import("@shikijs/langs/log"),
  "html": () => import("@shikijs/langs/html"),
  "xml": () => import("@shikijs/langs/xml"),
  "css": () => import("@shikijs/langs/css"),
  "scss": () => import("@shikijs/langs/scss"),
  "sass": () => import("@shikijs/langs/sass"),
  "less": () => import("@shikijs/langs/less"),
  "vue": () => import("@shikijs/langs/vue"),
  "svelte": () => import("@shikijs/langs/svelte"),
  "astro": () => import("@shikijs/langs/astro"),
  "sql": () => import("@shikijs/langs/sql"),
  "graphql": () => import("@shikijs/langs/graphql"),
  "proto": () => import("@shikijs/langs/proto"),
  "terraform": () => import("@shikijs/langs/terraform"),
  "hcl": () => import("@shikijs/langs/hcl"),
  "nix": () => import("@shikijs/langs/nix"),
  "dockerfile": () => import("@shikijs/langs/dockerfile"),
  "diff": () => import("@shikijs/langs/diff"),
  "elixir": () => import("@shikijs/langs/elixir"),
  "erlang": () => import("@shikijs/langs/erlang"),
  "haskell": () => import("@shikijs/langs/haskell"),
  "ocaml": () => import("@shikijs/langs/ocaml"),
  "clojure": () => import("@shikijs/langs/clojure"),
  "r": () => import("@shikijs/langs/r"),
  "julia": () => import("@shikijs/langs/julia"),
  "perl": () => import("@shikijs/langs/perl"),
  "viml": () => import("@shikijs/langs/viml"),
  "csv": () => import("@shikijs/langs/csv"),
  "makefile": () => import("@shikijs/langs/makefile"),
  "cmake": () => import("@shikijs/langs/cmake"),
};

/** A piece of a line: its text, color and font style (1 italic, 2 bold, 4 underline). */
export interface Tok {
  t: string;
  c?: string;
  s?: number;
}

/** Larger files are shown without colors. */
const MAX_CHARS = 400_000;

let core: Promise<HighlighterCore> | null = null;
let skinTheme: Promise<boolean> | null = null;
const loading = new Map<string, Promise<boolean>>();

function highlighter(): Promise<HighlighterCore> {
  core ??= createHighlighterCore({
    themes: [import("@shikijs/themes/github-light-default"), import("@shikijs/themes/github-dark-default")],
    langs: [],
    engine: createJavaScriptRegexEngine({ forgiving: true }),
  });
  return core;
}

/** Loads the plugin's color theme as "skin" (false when there is none or it doesn't load). */
export function loadSkinTheme(): Promise<boolean> {
  skinTheme ??= (async () => {
    try {
      const r = await fetch("/skin/theme.json");
      if (!r.ok) return false;
      const t = await r.json();
      await (await highlighter()).loadTheme({ ...t, name: "skin" });
      return true;
    } catch (e) {
      console.warn("couldn't load the skin's theme", e);
      return false;
    }
  })();
  return skinTheme;
}

async function language(h: HighlighterCore, lang: string): Promise<boolean> {
  if (h.getLoadedLanguages().includes(lang)) return true;
  const load = LANGS[lang];
  if (!load) return false;
  let p = loading.get(lang);
  if (!p) {
    p = load()
      .then((m) => h.loadLanguage(m.default))
      .then(() => true)
      .catch((e) => {
        console.warn(`couldn't load the ${lang} grammar`, e);
        return false;
      });
    loading.set(lang, p);
  }
  return p;
}

const cache = new Map<string, Tok[][]>();

function hash(s: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 0x01000193);
  return h >>> 0;
}

function plain(code: string): Tok[][] {
  return code.split("\n").map((t) => [{ t }]);
}

/** The code's lines as colored pieces. `theme` is "skin", "light" or "dark". */
export async function tokenize(code: string, lang: string, theme: string): Promise<Tok[][]> {
  if (code.length > MAX_CHARS) return plain(code);
  const key = `${theme}\0${lang}\0${code.length}\0${hash(code)}`;
  const hit = cache.get(key);
  if (hit) return hit;
  const h = await highlighter();
  if (!(await language(h, lang))) return plain(code);
  const name = theme === "skin" && (await loadSkinTheme()) ? "skin" : theme === "light" ? "github-light-default" : "github-dark-default";
  let lines: Tok[][];
  try {
    lines = h.codeToTokensBase(code, { lang, theme: name }).map((line) =>
      line.map((tok) => ({ t: tok.content, c: tok.color, s: tok.fontStyle || undefined })),
    );
  } catch (e) {
    console.warn("highlighting failed", e);
    return plain(code);
  }
  // Keep the last few files.
  if (cache.size > 24) cache.delete(cache.keys().next().value!);
  cache.set(key, lines);
  return lines;
}
