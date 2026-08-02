import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";

import { machineQuery, nodesQuery } from "../../api/client";
import { gaugeTone, gb, Meter } from "../MachineHealth";
import { cn } from "../../lib/utils";
import type { StreamStatus } from "../../hooks/useBnStream";

/** The live-stream dot: green (open), amber (connecting/lost), red (down). It pulses only while
 * the stream is actually open — a frozen dot on a dead stream would be the one lie the topbar
 * must never tell. */
function ConnDot({ status }: { status: StreamStatus }) {
  const tone: Record<StreamStatus, string> = {
    open: "bg-ok animate-pulse",
    connecting: "bg-warn",
    lost: "bg-warn",
    down: "bg-urgent",
  };
  return (
    <span
      className={cn("h-[7px] w-[7px] flex-none rounded-full", tone[status] ?? "bg-muted-foreground")}
      title={`live stream: ${status}`}
      aria-label={`live stream: ${status}`}
    />
  );
}

/** One topbar reading: a label, an optional meter, and a mono value. */
function Vital({
  label,
  children,
  title,
  className,
}: {
  label: string;
  children: React.ReactNode;
  title: string;
  className?: string;
}) {
  return (
    <span
      className={cn(
        "flex items-center gap-[7px] font-mono text-[11.5px] tabular-nums text-muted-foreground",
        className,
      )}
      title={title}
    >
      <span>{label}</span>
      {children}
    </span>
  );
}

/**
 * The 46px bar over the whole shell: who you are looking at (wordmark · live dot) on the left, how
 * the host is holding up on the right.
 *
 * The vitals used to live only on `/nodes`, which meant the one number that decides whether to
 * start another agent — is this machine already saturated? — was a navigation away from every
 * screen where you'd decide it. Here they are ambient: CPU and RAM as meters, agents in flight,
 * and nodes reachable, all patched live off the same queries the nodes view reads.
 *
 * Below the phone breakpoint the host and its meters drop and the counts stay: on a 380px screen
 * "3 agents, 3/3 nodes" is the part you can still act on.
 */
export function TopBar({
  status,
  onOpenPalette,
}: {
  status: StreamStatus;
  onOpenPalette: () => void;
}) {
  const machine = useQuery(machineQuery());
  const nodes = useQuery(nodesQuery());

  const m = machine.data;
  const all = nodes.data?.nodes ?? [];
  const up = all.filter((n) => n.status === "online").length;
  const host = all.find((n) => n.local)?.host;

  return (
    <header className="flex h-[46px] flex-none items-center gap-3.5 border-b border-border bg-background px-3.5 pt-[env(safe-area-inset-top)]">
      <Link to="/" className="flex items-baseline gap-px font-display text-[19px] tracking-tight">
        <b className="font-bold text-primary">bn</b>
        <span className="font-normal text-muted-foreground/70">/web</span>
      </Link>
      <ConnDot status={status} />

      <div className="ml-auto flex items-center gap-3.5">
        {host && (
          <span className="hidden font-mono text-[11.5px] text-muted-foreground/70 sm:inline">
            {host}
          </span>
        )}

        {m && (
          <>
            <Vital
              label="cpu"
              title="CPU load per core"
              className="hidden sm:flex"
            >
              <Meter
                frac={m.cpu.load_per_core}
                tone={gaugeTone(m.cpu.load_per_core, 0.7, 1.0)}
              />
              <span>
                {m.cpu.load1 == null ? "—" : m.cpu.load1.toFixed(1)}/{m.cpu.cores}
              </span>
            </Vital>
            <Vital label="ram" title="RAM used" className="hidden sm:flex">
              <Meter frac={m.ram.used_frac} tone={gaugeTone(m.ram.used_frac, 0.85, 0.95)} />
              <span>
                {gb(m.ram.used_mb)}/{gb(m.ram.total_mb)}
              </span>
            </Vital>
            <Vital label="agents" title="agents running">
              <span className="text-foreground">{m.agents.active}</span>
            </Vital>
          </>
        )}

        {all.length > 0 && (
          <Link
            to="/nodes"
            className="flex items-center gap-[7px] font-mono text-[11.5px] tabular-nums text-muted-foreground no-underline hover:text-foreground"
            title="nodes reachable — open the nodes view"
          >
            <span>nodes</span>
            <span className={cn(up < all.length ? "text-urgent" : "text-foreground")}>
              {up}/{all.length}
            </span>
          </Link>
        )}

        <button
          type="button"
          onClick={onOpenPalette}
          aria-label="open global search"
          className="flex h-[30px] items-center gap-2 rounded-lg border border-border bg-card px-2.5 text-[12.5px] text-muted-foreground transition-colors hover:border-input hover:text-foreground"
        >
          <span className="hidden sm:inline">Search</span>
          <kbd className="rounded border border-border bg-muted px-1.5 py-px font-mono text-[10.5px] text-muted-foreground">
            ⌘K
          </kbd>
        </button>
      </div>
    </header>
  );
}
