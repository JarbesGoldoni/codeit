import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";

export interface Option {
  id: string;
  label: string;
  detail?: string;
  group?: string;
  current?: boolean;
  right?: ReactNode;
}

/** A command-palette list: type to filter, ↑↓ to move, Enter to choose, Esc to close. */
export function Palette({
  title,
  options,
  onPick,
  onClose,
  placeholder = "Type to filter",
  empty = "Nothing matches.",
  footer,
}: {
  title: string;
  options: Option[];
  onPick: (o: Option) => void;
  onClose: () => void;
  placeholder?: string;
  empty?: string;
  footer?: ReactNode;
}) {
  const [filter, setFilter] = useState("");
  const shown = useMemo(() => {
    const words = filter.toLowerCase().split(/\s+/).filter(Boolean);
    return options.filter((o) =>
      words.every((w) => `${o.label} ${o.detail ?? ""} ${o.group ?? ""}`.toLowerCase().includes(w)),
    );
  }, [filter, options]);
  const [sel, setSel] = useState(() => Math.max(0, options.findIndex((o) => o.current)));
  const list = useRef<HTMLDivElement>(null);

  useEffect(() => {
    list.current?.querySelector(".on")?.scrollIntoView({ block: "nearest" });
  }, [sel, shown]);

  const at = Math.min(sel, shown.length - 1);
  return (
    <div className="overlay" onMouseDown={onClose}>
      <div className="palette" onMouseDown={(e) => e.stopPropagation()}>
        <div className="palettehead">{title}</div>
        <input
          autoFocus
          placeholder={placeholder}
          value={filter}
          onChange={(e) => {
            setFilter(e.target.value);
            setSel(0);
          }}
          onKeyDown={(e) => {
            if (e.key === "ArrowDown") {
              e.preventDefault();
              setSel((s) => Math.min(s + 1, shown.length - 1));
            } else if (e.key === "ArrowUp") {
              e.preventDefault();
              setSel((s) => Math.max(s - 1, 0));
            } else if (e.key === "Enter" && shown[at]) {
              e.preventDefault();
              onPick(shown[at]);
            } else if (e.key === "Escape") {
              e.preventDefault();
              onClose();
            }
          }}
        />
        <div className="palettelist" ref={list}>
          {shown.length === 0 && <div className="muted pad">{empty}</div>}
          {shown.map((o, i) => (
            <div key={o.id}>
              {o.group && o.group !== shown[i - 1]?.group && <div className="pgroup">{o.group}</div>}
              <button
                className={`prow${i === at ? " on" : ""}`}
                onMouseEnter={() => setSel(i)}
                onClick={() => onPick(o)}
              >
                <span className="plabel">
                  {o.current && <span className="check">✓ </span>}
                  {o.label}
                </span>
                {o.detail && <span className="pdetail">{o.detail}</span>}
                {o.right}
              </button>
            </div>
          ))}
        </div>
        {footer && <div className="palettefoot">{footer}</div>}
      </div>
    </div>
  );
}
