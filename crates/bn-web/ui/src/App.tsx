import { useEffect, useState } from "react";
import { NavLink, Outlet, useLocation } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { Search } from "lucide-react";
import { useBnStream } from "./hooks/useBnStream";
import {
  actionsQuery,
  attnQuery,
  cronQuery,
  fleetQuery,
  goalsQuery,
  orchestrateQuery,
} from "./api/client";
import { fleetEntries, needsYou } from "./lib/fleet";
import { ConnectionBanner } from "./components/ConnectionBanner";
import { CommandPalette } from "./components/CommandPalette";
import { AttentionIcon, CommandIcon, FleetIcon, MenuIcon, StartIcon } from "./components/icons";
import { cn } from "./lib/utils";
import type { StreamStatus } from "./hooks/useBnStream";

/** The 9px live-stream dot: green (open), amber (connecting/lost), red (down), grey otherwise —
 * with a soft matching ring on the settled states. */
function ConnDot({ status }: { status: StreamStatus }) {
  const tone: Record<StreamStatus, string> = {
    open: "bg-ok shadow-[0_0_0_3px_hsl(var(--ok)/0.16)]",
    connecting: "bg-warn",
    lost: "bg-warn shadow-[0_0_0_3px_hsl(var(--warn)/0.16)]",
    down: "bg-urgent shadow-[0_0_0_3px_hsl(var(--urgent)/0.16)]",
  };
  return (
    <span
      className={cn("h-[9px] w-[9px] flex-none rounded-full", tone[status] ?? "bg-muted-foreground")}
      title={`live stream: ${status}`}
    />
  );
}

/** Nav-badge tone → Tailwind token classes. */
const navBadgeTone: Record<string, string> = {
  red: "bg-urgent-muted text-urgent",
  amber: "bg-warn-muted text-warn",
  accent: "bg-primary/15 text-primary",
  grey: "bg-muted text-muted-foreground",
};

/** One nav destination. `badge` (when > 0) rides as a live count; `tone` colours it. */
interface NavItem {
  to: string;
  label: string;
  end?: boolean;
  badge?: number;
  tone?: string;
}

/** A titled group of nav destinations — the grouped IA (#522). */
interface NavGroup {
  title: string;
  items: NavItem[];
}

/**
 * The v2 dashboard shell (#522): an **action-centric command center** behind a grouped **sidebar**
 * that scales to the full surface set. Desktop = a fixed left sidebar (brand · pinned "needs you" +
 * live status · grouped, scannable nav) beside the content. Mobile = the sidebar collapses to a
 * hamburger **drawer**, with a minimal bottom bar kept for the highest-frequency jobs
 * (Command · Fleet · Start · Attention). One `useBnStream` owns the `EventSource` and patches the
 * react-query cache so every count stays live.
 */
export function App() {
  const { status } = useBnStream();
  const orch = useQuery(orchestrateQuery());
  const fleet = useQuery(fleetQuery());
  const attn = useQuery(attnQuery());
  const actions = useQuery(actionsQuery());
  const cron = useQuery(cronQuery());
  const goals = useQuery(goalsQuery());

  const [drawerOpen, setDrawerOpen] = useState(false);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const location = useLocation();

  // Close the mobile drawer on any navigation — a drawer that lingers over the new page is a bug.
  useEffect(() => {
    setDrawerOpen(false);
  }, [location.pathname]);

  // ⌘K / Ctrl-K toggles the global command palette from anywhere in the shell (#565).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setPaletteOpen((v) => !v);
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);

  const needAttention = needsYou(fleetEntries(orch.data, fleet.data)).length;
  const openAttn = attn.data?.items.length ?? 0;
  const actionable = actions.data?.count ?? 0;
  const urgentActions = (actions.data?.actions ?? []).filter((a) => a.severity === "urgent").length;
  const failingCron = (cron.data?.jobs ?? []).filter((j) => j.ok === false).length;
  const openGoals = goals.data?.daily.open ?? 0;

  const groups: NavGroup[] = [
    {
      title: "Command",
      items: [
        { to: "/", label: "Command center", end: true, badge: urgentActions, tone: "red" },
        { to: "/fleet", label: "Fleet", badge: needAttention, tone: "amber" },
        { to: "/start", label: "Start", tone: "accent" },
      ],
    },
    {
      title: "Act",
      items: [
        { to: "/attention", label: "Attention", badge: openAttn, tone: "red" },
        { to: "/research", label: "Research" },
      ],
    },
    {
      title: "Today",
      items: [
        { to: "/today", label: "Today" },
        { to: "/goals", label: "Goals", badge: openGoals, tone: "amber" },
      ],
    },
    {
      title: "Insights",
      items: [
        { to: "/insights", label: "Insights" },
        { to: "/cost", label: "Cost" },
        { to: "/repos", label: "Repos" },
      ],
    },
    {
      title: "Knowledge",
      items: [
        { to: "/notes", label: "Notes" },
        { to: "/handoff", label: "Handoff" },
        { to: "/skills", label: "Skills" },
      ],
    },
    {
      title: "System",
      items: [
        { to: "/cron", label: "Cron", badge: failingCron, tone: "red" },
      ],
    },
  ];

  // The minimal mobile bottom bar — the 4 highest-frequency jobs, badges preserved.
  const bottomTabs = [
    { to: "/", end: true, label: "Command", Icon: CommandIcon, badge: urgentActions },
    { to: "/fleet", end: false, label: "Fleet", Icon: FleetIcon, badge: needAttention },
    { to: "/start", end: false, label: "Start", Icon: StartIcon, badge: 0 },
    { to: "/attention", end: false, label: "Attention", Icon: AttentionIcon, badge: openAttn },
  ];

  return (
    <div className="min-h-[100dvh] md:flex">
      <aside
        aria-label="primary navigation"
        className={cn(
          "fixed inset-y-0 left-0 z-50 flex w-[268px] max-w-[84vw] flex-col gap-4 overflow-y-auto border-r border-border bg-card px-3 pb-5 pt-[calc(env(safe-area-inset-top)+1rem)] transition-[transform,visibility] duration-200 ease-out",
          drawerOpen ? "visible translate-x-0 shadow-pop" : "invisible -translate-x-full",
          "md:visible md:sticky md:top-0 md:h-[100dvh] md:max-w-[268px] md:flex-none md:translate-x-0 md:shadow-none",
        )}
      >
        <div className="flex items-center justify-between px-2">
          <span className="font-mono text-[1.1rem] font-extrabold tracking-tight text-primary">
            bn<span className="ml-0.5 font-normal text-muted-foreground/70">/web</span>
          </span>
          <ConnDot status={status} />
        </div>

        <button
          type="button"
          className="flex min-h-[44px] w-full items-center gap-2 rounded-md border border-border bg-transparent px-3 py-2 text-left text-sm font-semibold text-muted-foreground/70 hover:bg-accent hover:text-muted-foreground"
          onClick={() => setPaletteOpen(true)}
          aria-label="open global search"
        >
          <Search className="h-4 w-4 flex-none" />
          <span>Search…</span>
          <span className="ml-auto rounded-md border border-border px-1.5 py-px text-xs tabular-nums text-muted-foreground/70">
            ⌘K
          </span>
        </button>

        <NavLink
          to="/"
          end
          className="flex min-h-[44px] items-center gap-3 rounded-lg bg-primary/15 px-3 text-foreground no-underline hover:bg-accent"
        >
          <span className="text-3xl font-extrabold leading-none tabular-nums text-primary">
            {actionable}
          </span>
          <span className="text-sm leading-tight text-muted-foreground">
            {actionable === 1 ? "item needs you" : "items need you"}
            {urgentActions > 0 && (
              <em className="font-bold not-italic text-urgent"> · {urgentActions} urgent</em>
            )}
          </span>
        </NavLink>

        <nav className="flex flex-col gap-4">
          {groups.map((g) => (
            <div key={g.title} className="flex flex-col gap-0.5">
              <div className="mb-1 px-2 text-xs font-bold uppercase tracking-wider text-muted-foreground/70">
                {g.title}
              </div>
              {g.items.map((it) => (
                <NavLink key={it.to} to={it.to} end={it.end} className={navClass}>
                  <span>{it.label}</span>
                  {it.badge != null && it.badge > 0 && (
                    <span
                      className={cn(
                        "inline-flex h-5 min-w-[20px] items-center justify-center rounded-full px-1.5 text-xs font-extrabold tabular-nums",
                        navBadgeTone[it.tone ?? "grey"] ?? navBadgeTone.grey,
                      )}
                    >
                      {it.badge}
                    </span>
                  )}
                </NavLink>
              ))}
            </div>
          ))}
        </nav>
      </aside>

      {drawerOpen && (
        <div
          className="fixed inset-0 z-40 bg-black/55 backdrop-blur-[1px] md:hidden"
          onClick={() => setDrawerOpen(false)}
          aria-hidden="true"
        />
      )}

      <div className="min-h-[100dvh] md:min-w-0 md:flex-1">
        <header className="sticky top-0 z-20 flex h-[calc(52px+env(safe-area-inset-top))] items-center gap-3 border-b border-border bg-popover/90 px-4 pt-[env(safe-area-inset-top)] backdrop-blur-md backdrop-saturate-150 md:hidden">
          <button
            type="button"
            className="-ml-2 inline-flex h-11 w-11 items-center justify-center rounded-md border-0 bg-transparent text-muted-foreground hover:bg-accent hover:text-foreground"
            aria-label="open navigation"
            aria-expanded={drawerOpen}
            onClick={() => setDrawerOpen((v) => !v)}
          >
            <MenuIcon className="h-6 w-6" />
          </button>
          <span className="font-mono text-[1.1rem] font-extrabold tracking-tight text-primary">
            bn<span className="ml-0.5 font-normal text-muted-foreground/70">/web</span>
          </span>
          <span className="flex-1" />
          <button
            type="button"
            className="inline-flex h-11 w-11 items-center justify-center rounded-md border-0 bg-transparent text-muted-foreground hover:bg-accent hover:text-foreground"
            aria-label="open global search"
            onClick={() => setPaletteOpen(true)}
          >
            <Search className="h-5 w-5" />
          </button>
          <ConnDot status={status} />
        </header>

        <ConnectionBanner status={status} />

        <main className="mx-auto max-w-[760px] p-4 pb-[calc(58px+env(safe-area-inset-bottom)+1.5rem)] md:max-w-[1100px] md:p-6 md:pb-8">
          <Outlet />
        </main>

        <nav
          aria-label="primary"
          className="fixed inset-x-0 bottom-0 z-30 grid h-[calc(58px+env(safe-area-inset-bottom))] auto-cols-fr grid-flow-col border-t border-border bg-popover/95 pb-[env(safe-area-inset-bottom)] backdrop-blur-md backdrop-saturate-150 md:hidden"
        >
          {bottomTabs.map(({ to, end, label, Icon, badge }) => (
            <NavLink key={to} to={to} end={end} className={tabClass} aria-label={label}>
              <Icon className="block h-6 w-6" />
              <span>{label}</span>
              {badge > 0 && (
                <span
                  className="absolute left-[calc(50%+6px)] top-1 inline-flex h-[17px] min-w-[17px] items-center justify-center rounded-full border-2 border-popover bg-urgent px-1 text-[0.62rem] font-extrabold text-white"
                  aria-label={`${badge} need attention`}
                >
                  {badge}
                </span>
              )}
            </NavLink>
          ))}
        </nav>
      </div>

      <CommandPalette open={paletteOpen} onOpenChange={setPaletteOpen} />
    </div>
  );
}

function navClass({ isActive }: { isActive: boolean }): string {
  return cn(
    "flex min-h-[44px] items-center justify-between rounded-md px-3 py-2 text-sm font-semibold no-underline",
    isActive
      ? "bg-primary/15 text-primary"
      : "text-muted-foreground hover:bg-accent hover:text-foreground",
  );
}
function tabClass({ isActive }: { isActive: boolean }): string {
  return cn(
    "relative flex flex-col items-center justify-center gap-[3px] text-[0.66rem] font-semibold no-underline [-webkit-tap-highlight-color:transparent]",
    isActive ? "text-primary" : "text-muted-foreground/70",
  );
}
