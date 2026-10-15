import { useSkin } from "./skin";
import { fileIcon, folderIcon } from "./icons";

// Codicon-like fallbacks, drawn on a 16×16 grid, for when the skin has no product icons.
const PATHS: Record<string, string> = {
  files: "M4 1.5h5l3 3V12H4zM9 1.5V4.5h3M2 4v10.5h8",
  search: "M7 2.5a4.5 4.5 0 1 1 0 9a4.5 4.5 0 0 1 0-9zM10.3 10.3L14 14",
  "source-control": "M5 2.5v8M5 10.5a2 2 0 1 0 0 4a2 2 0 0 0 0-4zM11 3a2 2 0 1 0 0 4a2 2 0 0 0 0-4zM11 7c0 3-6 2-6 4",
  "git-commit": "M8 5.5a2.5 2.5 0 1 1 0 5a2.5 2.5 0 0 1 0-5zM1 8h4.5M10.5 8H15",
  "git-branch": "M5 2.5v8M5 10.5a2 2 0 1 0 0 4a2 2 0 0 0 0-4zM11 3a2 2 0 1 0 0 4a2 2 0 0 0 0-4zM11 7c0 3-6 2-6 4",
  folder: "M1.5 3.5h4.5l1.5 1.5h7v8.5h-13z",
  "chevron-right": "M6 3.5l4.5 4.5L6 12.5",
  "chevron-down": "M3.5 6l4.5 4.5L12.5 6",
  close: "M4 4l8 8M12 4l-8 8",
  comment: "M2 3h12v8H7l-3 3v-3H2z",
  "comment-discussion": "M2 3h12v8H7l-3 3v-3H2z",
  eye: "M1 8s2.5-4.5 7-4.5S15 8 15 8s-2.5 4.5-7 4.5S1 8 1 8zM8 6a2 2 0 1 1 0 4a2 2 0 0 1 0-4z",
  check: "M3 8.5l3 3L13 4.5",
  refresh: "M13 8a5 5 0 1 1-1.5-3.5M13 2.5v3h-3",
  history: "M2.5 8a5.5 5.5 0 1 0 1.6-3.9M2.5 2.5v2.6h2.6M8 5v3.5l2.5 1.5",
  "arrow-left": "M13 8H3M7 4L3 8l4 4",
  "settings-gear": "M8 5.5a2.5 2.5 0 1 1 0 5a2.5 2.5 0 0 1 0-5zM8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M3.4 12.6l1.4-1.4M11.2 4.8l1.4-1.4",
  pass: "M8 1.5a6.5 6.5 0 1 1 0 13a6.5 6.5 0 0 1 0-13zM5 8.2l2 2L11 6",
  "pass-filled": "M8 1.5a6.5 6.5 0 1 1 0 13a6.5 6.5 0 0 1 0-13zM5 8.2l2 2L11 6",
  "circle-large-outline": "M8 1.5a6.5 6.5 0 1 1 0 13a6.5 6.5 0 0 1 0-13z",
  code: "M5.5 4L1.5 8l4 4M10.5 4l4 4-4 4",
  "collapse-all": "M4 2.5h9.5V12M2.5 4.5h9v9h-9zM5 9h4",
  "list-tree": "M2 3h4M4 3v9h3M4 7.5h3M9 7.5h5M9 12h5",
  "go-to-file": "M4 1.5h5l3 3V14.5H4zM6.5 9h4M8.5 7l2 2-2 2",
  sparkle: "M8 1.5l1.6 4.9L14.5 8l-4.9 1.6L8 14.5l-1.6-4.9L1.5 8l4.9-1.6z",
};

/** An app icon: the skin's product icon when it has one, else a drawn one. */
export function Icon({ id, size = 16, title }: { id: string; size?: number; title?: string }) {
  const { product } = useSkin();
  const glyph = product?.icons[id];
  if (glyph) {
    return (
      <span className="picon" style={{ fontFamily: glyph.font, fontSize: size }} title={title} aria-hidden={!title}>
        {glyph.char}
      </span>
    );
  }
  const d = PATHS[id] ?? PATHS.code;
  return (
    <svg className="picon" viewBox="0 0 16 16" width={size} height={size} fill="none" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" strokeLinejoin="round">
      {title && <title>{title}</title>}
      <path d={d} />
    </svg>
  );
}

/** A file's icon from the skin's icon theme, or a plain page. */
export function FileIcon({ path }: { path: string }) {
  const { icons } = useSkin();
  const src = icons ? fileIcon(icons, path) : null;
  if (src) return <img className="ficon" src={src} alt="" draggable={false} />;
  return (
    <svg className="ficon plain" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.2">
      <path d="M4 1.5h5l3 3V14.5H4zM9 1.5V4.5h3" />
    </svg>
  );
}

export function FolderIcon({ path, open, root }: { path: string; open: boolean; root?: boolean }) {
  const { icons } = useSkin();
  const src = icons ? folderIcon(icons, path, open, root) : null;
  if (src) return <img className="ficon" src={src} alt="" draggable={false} />;
  return (
    <svg className="ficon plain" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.2">
      <path d={open ? "M1.5 3.5h4.5l1.5 1.5h6v2H4l-2.5 6.5zM4 7h11l-2.5 6.5h-11" : "M1.5 3.5h4.5l1.5 1.5h7v8.5h-13z"} />
    </svg>
  );
}
