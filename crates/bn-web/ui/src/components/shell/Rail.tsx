import { NavLink } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import {
  ArrowLeftRight,
  Bell,
  CircleDollarSign,
  Clock,
  FileText,
  Folder,
  HardDrive,
  LineChart,
  Microscope,
  Plus,
  Server,
  SquareTerminal,
  Sun,
  Target,
  Workflow,
  Zap,
} from "lucide-react";

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
import { cn } from "../../lib/utils";

/** Count-pill tone. `hot` is the one that means *a human is blocked*; everything else is neutral,
 * because a rail where four things glow red is a rail with no signal in it. */
type Tone = "hot" | "plain";

interface NavItem {
  to: string;
  label: string;
  Icon: typeof SquareTerminal;
  end?: boolean;
  count?: number;
  tone?: Tone;
}

interface NavGroup {
  title: string;
  items: NavItem[];
}

/**
 * Primary navigation — and, below 880px, the bottom tab bar. One element, two shapes.
 *
 * The old shell had two separate navs: a grouped sidebar hidden behind a hamburger on a phone, and
 * a four-entry bottom bar for the jobs that couldn't wait for two taps. That split meant eleven of
 * the fifteen destinations were reachable on mobile only by opening a drawer first. Here the rail
 * simply re-flows — column on a desktop, a scrollable row of icon+label tabs on a phone — so every
 * destination costs exactly one tap on every screen.
 *
 * Groups follow the clay prototype (Operate · Work · Insight · Knowledge), extended with the four
 * destinations the prototype's rail left out (Attention, Automation, Research, and Start as the
 * pinned button) so nothing that exists falls out of the nav.
 */
export function Rail() {
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

  const groups: NavGroup[] = [
    {
      title: "Operate",
      items: [
        {
          to: "/",
          label: "Today",
          Icon: Sun,
          end: true,
          count: todayTasks,
          tone: "plain",
        },
        {
          to: "/console",
          label: "Console",
          Icon: SquareTerminal,
          count: urgentActions,
          tone: "hot",
        },
        { to: "/fleet", label: "Fleet", Icon: Server, count: needAttention, tone: "hot" },
        { to: "/nodes", label: "Nodes", Icon: HardDrive, count: allNodes.length },
        { to: "/cron", label: "Cron", Icon: Clock, count: failingCron, tone: "hot" },
        {
          to: "/attention",
          label: "Attention",
          Icon: Bell,
          count: attn.data?.items.length ?? 0,
          tone: "hot",
        },
        { to: "/inbox", label: "Automation", Icon: Workflow },
      ],
    },
    {
      title: "Work",
      items: [
        { to: "/goals", label: "Goals", Icon: Target, count: goals.data?.daily.open ?? 0 },
        { to: "/repos", label: "Repos", Icon: Folder },
      ],
    },
    {
      title: "Insight",
      items: [
        { to: "/insights", label: "Insights", Icon: LineChart },
        { to: "/cost", label: "Cost", Icon: CircleDollarSign },
      ],
    },
    {
      title: "Knowledge",
      items: [
        { to: "/notes", label: "Notes", Icon: FileText },
        { to: "/skills", label: "Skills", Icon: Zap },
        { to: "/handoff", label: "Handoff", Icon: ArrowLeftRight },
        { to: "/research", label: "Research", Icon: Microscope },
      ],
    },
  ];

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
      {/* #1006: work only ever starts from a task, so the pinned verb is *New task*, not
          "Start an agent". The old button spawned a process from a free-text prompt and left no
          durable record — when the agent died, the intent died with it. */}
      <NavLink
        to="/"
        end
        className="mb-2.5 hidden h-[34px] items-center justify-center gap-[7px] rounded-[9px] bg-primary text-[13.5px] font-semibold text-primary-foreground no-underline shadow-card transition-[filter] hover:brightness-110 md:flex"
      >
        <Plus className="h-4 w-4" />
        New task
      </NavLink>

      {groups.map((g) => (
        <div key={g.title} className="contents md:block">
          <div className="hidden px-2 pb-1 pt-2.5 text-[10.5px] font-semibold uppercase tracking-[0.09em] text-muted-foreground/70 md:block">
            {g.title}
          </div>
          {g.items.map((it) => (
            <RailLink key={it.to} item={it} />
          ))}
        </div>
      ))}

      {/* Start has no rail row on a phone — it rides the console's own composer there — so the tab
          bar stays the *destinations*, not a mix of destinations and verbs. */}
      <NavLink
        to="/"
        end
        className={({ isActive }) => tabClass(isActive) + " md:hidden"}
        aria-label="New task"
      >
        <Plus className="h-[18px] w-[18px]" />
        <span>New</span>
      </NavLink>

      {host && (
        <div className="mt-auto hidden border-t border-border px-2 pb-0.5 pt-2.5 md:block">
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
function RailLink({ item }: { item: NavItem }) {
  const { to, label, Icon, end, count, tone } = item;
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
