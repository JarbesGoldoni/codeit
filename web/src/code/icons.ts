// File and folder icons from a VS Code file icon theme (the plugin's skin), picked the way
// VS Code picks them.

import { languageOf } from "./languages";

export interface IconTheme {
  iconDefinitions: Record<string, { iconPath?: string }>;
  file?: string;
  folder?: string;
  folderExpanded?: string;
  rootFolder?: string;
  rootFolderExpanded?: string;
  fileExtensions?: Record<string, string>;
  fileNames?: Record<string, string>;
  folderNames?: Record<string, string>;
  folderNamesExpanded?: Record<string, string>;
  languageIds?: Record<string, string>;
  hidesExplorerArrows?: boolean;
}

function url(theme: IconTheme, id: string | undefined): string | null {
  const p = id ? theme.iconDefinitions[id]?.iconPath : undefined;
  return p ? `/skin/${p.replace(/^\.?\//, "")}` : null;
}

function lower<T>(m: Record<string, T> | undefined, key: string): T | undefined {
  if (!m) return undefined;
  return m[key] ?? m[key.toLowerCase()];
}

export function fileIcon(theme: IconTheme, path: string): string | null {
  const name = path.split("/").pop()!;
  const byName = lower(theme.fileNames, name);
  if (byName) return url(theme, byName);
  // "a.test.ts": "test.ts", then "ts".
  const parts = name.toLowerCase().split(".");
  for (let i = 1; i < parts.length; i++) {
    const id = theme.fileExtensions?.[parts.slice(i).join(".")];
    if (id) return url(theme, id);
  }
  const lang = theme.languageIds?.[languageOf(path)];
  return url(theme, lang ?? theme.file);
}

export function folderIcon(theme: IconTheme, path: string, open: boolean, root = false): string | null {
  if (root) return url(theme, open ? theme.rootFolderExpanded ?? theme.folderExpanded : theme.rootFolder ?? theme.folder);
  const name = path.split("/").pop()!;
  const id = open ? lower(theme.folderNamesExpanded, name) : lower(theme.folderNames, name);
  return url(theme, id ?? (open ? theme.folderExpanded : theme.folder));
}
