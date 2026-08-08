import { NavLink } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { Settings } from "lucide-react";

import {
  actionsQuery,
  attnQuery,
  cronQuery,
  fleetQuery,
  goalsQuery,
  nodesQuery,
  orchestrateQuery,
  tasksQuery,
} from "../../api/client";
import { fleetEntries, needsYou } from "../../lib/fleet";
import { useExperimental } from "../../lib/experimental";
import { NAV_GROUPS, type NavItem } from "../../lib/nav";
import { cn } from "../../lib/utils";

/**
 * Primary navigation — and, below 880px, the bottom tab bar. One element, two shapes.
 *
 * The old shell had two separate navs: a grouped sidebar hidden behind a hamburger on a phone, and
 * a four-entry bottom bar for the jobs that couldn't wait for two taps. That split meant eleven of
 * the fifteen destinations were reachable on mobile only by opening a drawer first. Here the rail
 * simply re-flows — column on a desktop, a scrollable row of icon+label tabs on a phone — so every
 * destination costs exactly one tap on every screen.
 *
 * The destinations themselves live in `lib/nav`, because `/settings` renders the same list as
 * switches and the two must not drift. The rail adds what only it knows: the live count on each
 * pill.
 *
 * The rail carries destinations only — starting a task is a verb the Today screen owns, not a nav
 * entry — plus the settings link, which is a destination like any other.
 *
 * Optional destinations are behind `?experimental` (see `lib/experimental`). Until a screen is
 * tested end to end it stays out of the rail; the default rail carries Today + Cron, plus the
 * settings link. Each optional tab carries the `exp` key that reveals it, so `?experimental=fleet`
 * brings Fleet back and `/settings` flips the same keys without the URL syntax. The routes still
 * resolve either way; optional routes are simply not advertised.
 */
export function Rail() {
  const experimental = useExperimental();
  const orch = useQuery(orchestrateQuery());
  const fleet = useQuery(fleetQuery());
  const attn = useQuery(attnQuery());
  const actions = useQuery(actionsQuery());
  const cron = useQuery(cronQuery());
  const goals = useQuery(goalsQuery());
  const nodes = useQuery(nodesQuery());
  const tasks = useQuery(tasksQuery("today"));

  const entries = fleetEntries(orch.data, fleet.data);
  const needAttention = needsYou(entries).length;
  const urgentActions = (actions.data?.actions ?? []).filter((a) => a.severity === "urgent").length;
  const failingCron = (cron.data?.jobs ?? []).filter((j) => j.ok === false).length;
  const allNodes = nodes.data?.nodes ?? [];
  const todayTasks = tasks.data?.count ?? 0;
  const host = allNodes.find((n) => n.local)?.host;

  // The live pill on each destination, by route. A tab with no entry here simply has no count.
  const counts: Record<string, number> = {
    "/": todayTasks,
    "/console": urgentActions,
    "/fleet": needAttention,
    "/nodes": allNodes.length,
    "/cron": failingCron,
    "/attention": attn.data?.items.length ?? 0,
    "/goals": goals.data?.daily.open ?? 0,
  };

  // Default tabs plus the optional tabs `?experimental=` named, and only the group headings that
  // still label something. A lone "Operate" caption over a single tab is noise, not structure.
  const groups = NAV_GROUPS.map((g) => ({
    ...g,
    items: g.items.filter((i) => experimental.has(i.exp)),
  })).filter((g) => g.items.length > 0);

  return (
    <nav
      aria-label="primary"
      className={cn(
        // Phone: a scrollable row of tabs pinned under the content (order-2 in the shell's flex
        // column). Desktop: the 12.5rem column it has always wanted to be.
        "order-2 flex flex-none flex-row items-stretch gap-0 overflow-x-auto border-t border-border bg-background px-1.5 pb-[calc(4px+env(safe-area-inset-bottom))] pt-1",
        "md:order-none md:flex-col md:gap-1 md:overflow-x-visible md:overflow-y-auto md:border-r md:border-t-0 md:px-2.5 md:py-3",
      )}
    >
      {groups.map((g) => (
        <div key={g.title} className="contents md:block">
          {experimental.any && (
            <div className="hidden px-2 pb-1 pt-2.5 text-[10.5px] font-semibold uppercase tracking-[0.09em] text-muted-foreground/70 md:block">
              {g.title}
            </div>
          )}
          {g.items.map((it) => (
            <RailLink key={it.to} item={it} count={counts[it.to]} />
          ))}
        </div>
      ))}

      {/* Settings is never gated: it is the screen that owns the gate, so hiding it behind the flag
          would leave the query string as the only way back. On a desktop it sits at the foot of the
          rail, above the host block; on a phone it is just another tab. */}
      <NavLink
        to="/settings"
        className={({ isActive }) =>
          cn(
            tabClass(isActive),
            "md:mt-auto md:h-8 md:w-full md:flex-row md:items-center md:justify-start md:gap-[9px] md:rounded-[7px] md:px-2 md:py-0 md:text-[13.5px] md:font-medium",
            isActive
              ? "md:bg-primary/[0.13] md:text-primary"
              : "md:text-muted-foreground md:hover:bg-card md:hover:text-foreground",
          )
        }
      >
        <Settings className="h-[18px] w-[18px] flex-none opacity-[0.85] md:h-[15px] md:w-[15px]" />
        <span>Settings</span>
      </NavLink>

      {host && (
        <div className="hidden border-t border-border px-2 pb-0.5 pt-2.5 md:mt-2.5 md:block">
          <div className="font-mono text-[11px] text-muted-foreground">{host}</div>
          <div className="mt-1 text-[10.5px] leading-snug text-muted-foreground/70">
            {allNodes.length} node{allNodes.length === 1 ? "" : "s"} · {entries.length} agent
            {entries.length === 1 ? "" : "s"}
          </div>
        </div>
      )}
    </nav>
  );
}

/** One destination, in both shapes: icon-above-label tab on a phone, icon-beside-label row with a
 * trailing count pill on a desktop. */
function RailLink({ item, count }: { item: NavItem; count?: number }) {
  const { to, label, Icon, end, tone } = item;
  const showCount = count != null && count > 0;
  return (
    <NavLink
      to={to}
      end={end}
      className={({ isActive }) =>
        cn(
          tabClass(isActive),
          "md:h-8 md:w-full md:flex-row md:items-center md:justify-start md:gap-[9px] md:rounded-[7px] md:px-2 md:py-0 md:text-[13.5px] md:font-medium",
          isActive
            ? "md:bg-primary/[0.13] md:text-primary"
            : "md:text-muted-foreground md:hover:bg-card md:hover:text-foreground",
        )
      }
    >
      <Icon className="h-[18px] w-[18px] flex-none opacity-[0.85] md:h-[15px] md:w-[15px]" />
      <span>{label}</span>
      {showCount && (
        <span
          className={cn(
            // Phone: a corner badge over the icon. Desktop: a pill at the end of the row.
            "absolute right-1 top-0.5 rounded-full px-[5px] font-mono text-[9px] tabular-nums",
            "md:static md:ml-auto md:px-[7px] md:py-px md:text-[11px]",
            tone === "hot"
              ? "bg-urgent-muted font-semibold text-urgent"
              : "bg-muted text-muted-foreground",
          )}
          aria-label={`${count} ${tone === "hot" ? "need attention" : ""}`.trim()}
        >
          {count}
        </span>
      )}
    </NavLink>
  );
}

/** The phone-tab base shared by every rail entry (desktop overrides ride on top of it). */
function tabClass(isActive: boolean): string {
  return cn(
    "relative flex flex-none flex-col items-center justify-center gap-[3px] rounded-[9px] px-3 py-[7px] text-[10.5px] font-medium no-underline [-webkit-tap-highlight-color:transparent]",
    isActive ? "text-primary" : "text-muted-foreground",
  );
}
