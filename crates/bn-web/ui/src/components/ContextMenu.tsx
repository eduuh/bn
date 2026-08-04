import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { MouseEvent as ReactMouseEvent, ReactNode } from "react";
import { createPortal } from "react-dom";
import type { LucideIcon } from "lucide-react";

import { cn } from "../lib/utils";

/**
 * A right-click action on a card — the redesign's primary affordance (#993). Kept deliberately
 * small: a label, an optional icon, the thing it does, and two flags (`danger` for destructive
 * verbs, `separatorBefore` to fence a group off). Items whose action has no server endpoint yet
 * simply aren't added by the caller, so the menu only ever offers things that actually work.
 */
export interface ContextMenuItem {
  label: ReactNode;
  icon?: LucideIcon;
  onSelect: () => void;
  danger?: boolean;
  disabled?: boolean;
  separatorBefore?: boolean;
}

/**
 * A headless context-menu: right-click the element carrying `onContextMenu` and a portalled menu
 * opens at the cursor, clamped to the viewport, dismissed on outside-click / Escape / scroll.
 *
 * It is a hook rather than a wrapper component so a row can keep being an `<li>` (or a `<div>`) with
 * its own layout — you spread `onContextMenu` onto the element and render `node` beside its content.
 */
export function useContextMenu(items: ContextMenuItem[], ariaLabel?: string) {
  const [pos, setPos] = useState<{ x: number; y: number } | null>(null);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!pos) return;
    const close = () => setPos(null);
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setPos(null);
    };
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setPos(null);
    };
    window.addEventListener("scroll", close, true);
    window.addEventListener("resize", close);
    window.addEventListener("keydown", onKey);
    window.addEventListener("mousedown", onDown);
    return () => {
      window.removeEventListener("scroll", close, true);
      window.removeEventListener("resize", close);
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("mousedown", onDown);
    };
  }, [pos]);

  // Clamp into the viewport once the menu has a measured size.
  useLayoutEffect(() => {
    if (!pos || !ref.current) return;
    const r = ref.current.getBoundingClientRect();
    const x = Math.max(8, Math.min(pos.x, window.innerWidth - r.width - 8));
    const y = Math.max(8, Math.min(pos.y, window.innerHeight - r.height - 8));
    ref.current.style.left = `${x}px`;
    ref.current.style.top = `${y}px`;
  }, [pos]);

  const onContextMenu = (e: ReactMouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setPos({ x: e.clientX, y: e.clientY });
  };

  const node =
    pos && items.length
      ? createPortal(
          <div
            ref={ref}
            role="menu"
            aria-label={ariaLabel}
            className="fixed z-50 min-w-[12rem] rounded-lg border border-input bg-popover p-1 text-popover-foreground shadow-xl"
            style={{ left: pos.x, top: pos.y }}
          >
            {items.map((it, i) => (
              <div key={i}>
                {it.separatorBefore && <div className="my-1 h-px bg-border" />}
                <button
                  type="button"
                  role="menuitem"
                  disabled={it.disabled}
                  onClick={() => {
                    setPos(null);
                    it.onSelect();
                  }}
                  className={cn(
                    "flex w-full items-center gap-2.5 rounded-md px-2.5 py-1.5 text-left text-sm transition-colors disabled:pointer-events-none disabled:opacity-50",
                    it.danger ? "text-urgent hover:bg-urgent-muted" : "text-foreground hover:bg-accent",
                  )}
                >
                  {it.icon ? <it.icon className="h-4 w-4 shrink-0 opacity-80" /> : null}
                  <span className="min-w-0 flex-1">{it.label}</span>
                </button>
              </div>
            ))}
          </div>,
          document.body,
        )
      : null;

  return { onContextMenu, node };
}
