// The plugin's skin (crates/codeit/src/skin.rs): a VS Code color theme for the whole app, its
// file icons and its product icons. Without one, codeit keeps its own look.

import { useSyncExternalStore } from "react";
import type { SkinInfo } from "../protocol";
import type { IconTheme } from "./icons";

interface ProductIcons {
  /** Icon id ("files", "git-commit"…) → font family and character. */
  icons: Record<string, { font: string; char: string }>;
}

interface SkinState {
  name: string | null;
  /** The theme's colors are applied (the viewer can turn them off). */
  colors: boolean;
  hasColors: boolean;
  dark: boolean;
  icons: IconTheme | null;
  product: ProductIcons | null;
}

let state: SkinState = { name: null, colors: false, hasColors: false, dark: false, icons: null, product: null };
const listeners = new Set<() => void>();
let themeColors: Record<string, string> = {};
let loaded = false;

function set(patch: Partial<SkinState>) {
  state = { ...state, ...patch };
  for (const l of listeners) l();
}

export function useSkin(): SkinState {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => state,
  );
}

function wantColors(): boolean {
  try {
    return localStorage.getItem("codeit.skin") !== "off";
  } catch {
    return true;
  }
}

/** Loads the skin the server announced (once). */
export async function loadSkin(info: SkinInfo | null) {
  if (!info || loaded) return;
  loaded = true;
  set({ name: info.name });
  const get = async (path: string) => {
    try {
      const r = await fetch(`/skin/${path}`);
      return r.ok ? await r.json() : null;
    } catch {
      return null;
    }
  };
  const [theme, icons, product] = await Promise.all([
    info.theme ? get("theme.json") : null,
    info.icons ? get("icons.json") : null,
    info.product_icons ? get("product-icons.json") : null,
  ]);
  if (theme?.colors) {
    themeColors = theme.colors;
    set({ hasColors: true, dark: theme.type !== "light" });
    setColors(wantColors());
  }
  if (icons) set({ icons });
  if (product) set({ product: productIcons(product) });
}

export function setColors(on: boolean) {
  try {
    localStorage.setItem("codeit.skin", on ? "on" : "off");
  } catch {
    // For this tab only.
  }
  const root = document.documentElement;
  for (const [v] of VARS) root.style.removeProperty(v);
  if (on && state.hasColors) {
    // A highlight the same color as the background (some themes mark selection by text color
    // only) would be invisible: the next color in the list is taken instead.
    const bg = norm(themeColors["editor.background"]);
    for (const [v, keys] of VARS) {
      const c = keys.map((k) => themeColors[k]).find((c) => c && (BACKGROUNDS.has(v) || norm(c) !== bg));
      if (c) root.style.setProperty(v, c);
    }
    root.dataset.theme = state.dark ? "dark" : "light";
    root.style.colorScheme = state.dark ? "dark" : "light";
  } else {
    delete root.dataset.theme;
    root.style.removeProperty("color-scheme");
  }
  set({ colors: on && state.hasColors });
}

const BACKGROUNDS = new Set(["--bg", "--sidebar", "--surface", "--raised", "--editor-bg", "--tab-bg", "--tab-active", "--code"]);

/** `#0a0a0aff` and `#0A0A0A` alike. */
function norm(c: string | undefined): string {
  const x = (c ?? "").toLowerCase();
  return x.length === 9 && x.endsWith("ff") ? x.slice(0, 7) : x;
}

/** codeit's colors and the VS Code theme colors they take, first one the theme has. */
const VARS: [string, string[]][] = [
  ["--bg", ["editor.background"]],
  ["--sidebar", ["sideBar.background", "editor.background"]],
  ["--surface", ["input.background", "editorWidget.background"]],
  ["--raised", ["editorWidget.background", "menu.background", "dropdown.background"]],
  ["--border", ["editorGroup.border", "panel.border", "sideBar.border", "widget.border"]],
  ["--border-strong", ["input.border", "dropdown.border", "editorWidget.border"]],
  ["--text", ["editor.foreground", "foreground"]],
  ["--muted", ["descriptionForeground", "sideBar.foreground", "foreground"]],
  ["--faint", ["editorLineNumber.foreground", "disabledForeground"]],
  ["--accent", ["button.background", "focusBorder", "textLink.foreground"]],
  ["--accent-ink", ["button.foreground"]],
  ["--accent-soft", ["list.activeSelectionBackground", "editor.selectionBackground"]],
  ["--user", ["editor.lineHighlightBackground", "list.hoverBackground", "input.background"]],
  ["--code", ["textCodeBlock.background", "editorWidget.background"]],
  ["--ok", ["gitDecoration.addedResourceForeground", "terminal.ansiGreen"]],
  ["--warn", ["editorWarning.foreground", "gitDecoration.modifiedResourceForeground"]],
  ["--bad", ["editorError.foreground", "gitDecoration.deletedResourceForeground"]],
  ["--add-bg", ["diffEditor.insertedLineBackground", "diffEditor.insertedTextBackground"]],
  ["--del-bg", ["diffEditor.removedLineBackground", "diffEditor.removedTextBackground"]],
  ["--editor-bg", ["editor.background"]],
  ["--editor-fg", ["editor.foreground"]],
  ["--line-no", ["editorLineNumber.foreground"]],
  ["--line-no-active", ["editorLineNumber.activeForeground"]],
  ["--line-hl", ["editor.lineHighlightBackground"]],
  ["--sel-bg", ["editor.selectionBackground"]],
  ["--find-bg", ["editor.findMatchHighlightBackground", "editor.wordHighlightBackground"]],
  ["--tree-sel", ["list.activeSelectionBackground"]],
  ["--tree-sel-fg", ["list.activeSelectionForeground"]],
  ["--tree-hover", ["list.hoverBackground"]],
  ["--tree-guide", ["tree.indentGuidesStroke"]],
  ["--tab-bg", ["tab.inactiveBackground", "editorGroupHeader.tabsBackground"]],
  ["--tab-active", ["tab.activeBackground"]],
  ["--tab-border", ["tab.activeBorder", "focusBorder"]],
  ["--git-mod", ["gitDecoration.modifiedResourceForeground"]],
  ["--git-add", ["gitDecoration.untrackedResourceForeground", "gitDecoration.addedResourceForeground"]],
  ["--git-del", ["gitDecoration.deletedResourceForeground"]],
  ["--scroll", ["scrollbarSlider.background"]],
  ["--scroll-hover", ["scrollbarSlider.hoverBackground"]],
];

function productIcons(theme: {
  fonts?: { id: string; src: { path: string; format: string }[] }[];
  iconDefinitions?: Record<string, { fontCharacter: string; fontId?: string }>;
}): ProductIcons {
  const fonts = theme.fonts ?? [];
  const css = fonts
    .map((f) => {
      const src = f.src.map((s) => `url("/skin/${s.path.replace(/^\.?\//, "")}") format("${s.format}")`).join(", ");
      return `@font-face { font-family: "${f.id}"; src: ${src}; font-display: block; }`;
    })
    .join("\n");
  const style = document.createElement("style");
  style.textContent = css;
  document.head.appendChild(style);
  const icons: ProductIcons["icons"] = {};
  for (const [id, d] of Object.entries(theme.iconDefinitions ?? {})) {
    const hex = d.fontCharacter.replace(/^\\+/, "");
    const code = parseInt(hex, 16);
    if (!Number.isNaN(code)) icons[id] = { font: d.fontId ?? fonts[0]?.id ?? "", char: String.fromCodePoint(code) };
  }
  return { icons };
}
