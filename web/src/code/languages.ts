// File names to VS Code's language ids (also Shiki's grammar names, where it has one).

const BY_EXT: Record<string, string> = {
  ts: "typescript", mts: "typescript", cts: "typescript", tsx: "tsx",
  js: "javascript", mjs: "javascript", cjs: "javascript", jsx: "jsx",
  json: "json", jsonc: "jsonc", json5: "json5",
  rs: "rust", py: "python", pyi: "python", go: "go", java: "java", kt: "kotlin", kts: "kotlin",
  scala: "scala", groovy: "groovy", gradle: "groovy",
  c: "c", h: "c", cc: "cpp", cpp: "cpp", cxx: "cpp", hpp: "cpp", hh: "cpp", cs: "csharp",
  swift: "swift", m: "objective-c", rb: "ruby", php: "php", lua: "lua", dart: "dart", zig: "zig",
  sh: "shellscript", bash: "shellscript", zsh: "shellscript", fish: "fish", ps1: "powershell",
  bat: "bat", cmd: "bat",
  yml: "yaml", yaml: "yaml", toml: "toml", ini: "ini", cfg: "ini", properties: "properties",
  env: "dotenv",
  md: "markdown", mdx: "mdx", rst: "rst", txt: "plaintext", log: "log",
  html: "html", htm: "html", xml: "xml", svg: "xml", plist: "xml", xsd: "xml",
  css: "css", scss: "scss", sass: "sass", less: "less",
  vue: "vue", svelte: "svelte", astro: "astro",
  sql: "sql", graphql: "graphql", gql: "graphql", proto: "proto",
  tf: "terraform", hcl: "hcl", nix: "nix",
  dockerfile: "dockerfile", diff: "diff", patch: "diff",
  ex: "elixir", exs: "elixir", erl: "erlang", hs: "haskell", ml: "ocaml", clj: "clojure",
  r: "r", jl: "julia", pl: "perl", vim: "viml",
  csv: "csv", lock: "plaintext",
};

const BY_NAME: Record<string, string> = {
  dockerfile: "dockerfile",
  makefile: "makefile",
  gnumakefile: "makefile",
  "cmakelists.txt": "cmake",
  ".gitignore": "ignore",
  ".dockerignore": "ignore",
  ".env": "dotenv",
  "cargo.lock": "toml",
  ".bashrc": "shellscript",
  ".zshrc": "shellscript",
  ".profile": "shellscript",
  jenkinsfile: "groovy",
};

export function languageOf(path: string): string {
  const name = path.split("/").pop()!.toLowerCase();
  if (BY_NAME[name]) return BY_NAME[name];
  if (name.startsWith(".env.")) return "dotenv";
  if (name.startsWith("dockerfile.")) return "dockerfile";
  const ext = name.includes(".") ? name.split(".").pop()! : "";
  return BY_EXT[ext] ?? "plaintext";
}
