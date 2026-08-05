import { useEffect, useRef, useState } from "react";
import { Ban, Check, ChevronDown } from "lucide-react";
import type { HarnessOption } from "../api/types";
import { useHarnessSwitch } from "../hooks/useHarnessSwitch";
import { cn } from "../lib/utils";

/**
 * The harness control (#501), everywhere: the running harness as a chip, with the alternatives one
 * tap behind it. Switching harness stops the agent and relaunches it — rare and destructive — so it
 * earns a chip, not the permanent segmented block that used to take ~45% of a fleet card's height
 * and a whole sidebar card on the agent page. Switch semantics live in [`useHarnessSwitch`].
 *
 * The chip reads as identity, so it sits next to the branch on the agent header and in the meta row
 * on a card — the same fact at three scales, one component.
 *
 * The whole control stops propagation and cancels default so it works inside a card's navigation
 * link. Closes on outside click and on Escape. Any surface hosting it must not clip overflow.
 */
export function HarnessMenu({
  target,
  current,
  harnesses,
}: {
  target: string;
  current?: string;
  harnesses?: HarnessOption[];
}) {
  const { options, pick, titleFor, pending } = useHarnessSwitch(target, current, harnesses);
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (!root.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  // Nothing to label until the supervisor has read a harness off the session; and a single-option
  // machine has nothing to switch to, so the chip degrades to a plain label.
  if (!current) return null;
  const switchable = options.length > 1;

  const chip = (
    <>
      {current}
      {switchable && <ChevronDown className="h-3 w-3 opacity-60" aria-hidden="true" />}
    </>
  );

  const chipClass =
    "inline-flex items-center gap-1 rounded-md bg-muted px-1.5 py-0.5 text-xs text-muted-foreground";

  if (!switchable) {
    return <span className={chipClass}>{chip}</span>;
  }

  return (
    <div ref={root} className="relative inline-flex">
      <button
        type="button"
        aria-haspopup="menu"
        aria-expanded={open}
        disabled={pending}
        title={`Running under ${current ?? "an unknown harness"} — switch harness`}
        className={cn(
          chipClass,
          "transition-colors hover:bg-accent hover:text-foreground",
          pending && "opacity-60",
        )}
        onClick={(e) => {
          e.preventDefault();
          e.stopPropagation();
          setOpen((v) => !v);
        }}
      >
        {chip}
      </button>

      {open && (
        <div
          role="menu"
          aria-label="Switch harness"
          // Same surface as the Select dropdown — a popover that shares the card's own background
          // reads as part of the card rather than above it.
          className="absolute left-0 top-full z-30 mt-1 w-52 overflow-hidden rounded-md border border-border bg-popover text-popover-foreground shadow-pop"
          onClick={(e) => e.stopPropagation()}
        >
          {options.map((opt) => {
            const active = opt.name === current;
            return (
              <button
                key={opt.name}
                type="button"
                role="menuitem"
                aria-checked={active}
                disabled={pending || (!opt.available && !active)}
                title={titleFor(opt)}
                className={cn(
                  "flex min-h-[2.25rem] w-full items-center gap-2 px-2.5 py-1.5 text-left text-sm transition-colors",
                  active
                    ? "text-foreground"
                    : "text-muted-foreground hover:bg-accent hover:text-foreground",
                  !opt.available && !active && "opacity-60",
                )}
                onClick={(e) => {
                  e.preventDefault();
                  e.stopPropagation();
                  setOpen(false);
                  pick(opt);
                }}
              >
                {active ? (
                  <Check className="h-3.5 w-3.5 shrink-0 text-ok" aria-hidden="true" />
                ) : (
                  <span className="h-3.5 w-3.5 shrink-0" aria-hidden="true" />
                )}
                {opt.name}
                {!opt.available && (
                  <Ban className="ml-auto h-3.5 w-3.5 shrink-0" aria-hidden="true" />
                )}
              </button>
            );
          })}
          <p className="border-t border-border px-2.5 py-1.5 text-xs text-muted-foreground">
            Relaunches in place — worktree, branch and note are kept.
          </p>
        </div>
      )}
    </div>
  );
}
