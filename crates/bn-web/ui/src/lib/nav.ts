import {
  ListOrdered,
  ArrowLeftRight,
  Bell,
  Bot,
  Clock,
  FileText,
  Folder,
  HardDrive,
  LineChart,
  Microscope,
  SquareTerminal,
  Sun,
  Target,
  Users,
  Workflow,
  Zap,
  type LucideIcon,
} from "lucide-react";

/**
 * The shell's destinations, in nav order — the one list that says which screens exist, what they
 * are called, and which `?experimental=` key reveals optional ones.
 *
 * It lives here rather than inside [`Rail`] because two surfaces now need it and they must not
 * disagree: the rail renders it as navigation, and `/settings` renders it as a list of switches.
 * A tab added to the rail but missing from the settings list would be a tab you could only reach
 * by hand-editing the URL, which is the exact problem the settings page exists to solve.
 *
 * What stays out of here is anything *live*: the count pills are per-render query data, so the rail
 * still owns those and looks them up by `to`.
 */
export interface NavItem {
  to: string;
  label: string;
  Icon: LucideIcon;
  /** Exact-match the route (only `/`, which every other path is a prefix of). */
  end?: boolean;
  /** Count-pill tone. `hot` is the one that means *a human is blocked*; everything else is
   * neutral, because a rail where four things glow red is a rail with no signal in it. */
  tone?: "hot" | "plain";
  /** The `?experimental=` key that reveals this tab. Absent means ungated/default. */
  exp?: string;
  /** One line for the settings page: what this screen is, for someone deciding whether to show it.
   * The rail never renders it. */
  blurb?: string;
}

export interface NavGroup {
  title: string;
  items: NavItem[];
}

/** Groups follow the clay prototype (Operate · Work · Insight · Knowledge), extended with the
 * destinations the prototype's rail left out so nothing that exists falls out of the nav. */
export const NAV_GROUPS: NavGroup[] = [
  {
    title: "Operate",
    items: [
      {
        to: "/",
        label: "Today",
        Icon: Sun,
        end: true,
        tone: "plain",
        blurb: "The task-first home. Always on — it is the default surface.",
      },
      {
        to: "/queue",
        label: "Queue",
        Icon: ListOrdered,
        tone: "hot",
        exp: "queue",
        blurb:
          "The machine-wide work queue: what is waiting, why each held item is held, what each node can run, and the pause control.",
      },
      {
        to: "/console",
        label: "Console",
        Icon: SquareTerminal,
        tone: "hot",
        exp: "console",
        blurb: "The dense three-pane operator view: focus, queue and act.",
      },
      {
        to: "/nodes",
        label: "Nodes",
        Icon: HardDrive,
        exp: "nodes",
        blurb: "The machines behind the fleet — CPU, RAM and reachability per host.",
      },
      {
        to: "/cron",
        label: "Cron",
        Icon: Clock,
        tone: "hot",
        blurb:
          "The default automation surface: scheduled Markdown agents, cron jobs, and the health of each job's last run.",
      },
      {
        to: "/staff",
        label: "Staff",
        Icon: Users,
        exp: "staff",
        blurb:
          "The staff roster and its briefing cards: per-agent health, backpressure pauses, and the feed — readable in place, which is what resumes a paused agent.",
      },
      {
        to: "/attention",
        label: "Attention",
        Icon: Bell,
        tone: "hot",
        exp: "attention",
        blurb: "Everything waiting on a decision from you.",
      },
      {
        to: "/inbox",
        label: "Automation",
        Icon: Workflow,
        exp: "inbox",
        blurb: "Plan reviews, the automation hub, and the completed history.",
      },
    ],
  },
  {
    title: "Work",
    items: [
      {
        to: "/goals",
        label: "Goals",
        Icon: Target,
        exp: "goals",
        blurb: "Daily and quarterly goals, and the notes connected to them.",
      },
      {
        to: "/repos",
        label: "Repos",
        Icon: Folder,
        exp: "repos",
        blurb: "Per-repo rollups: sessions, worktree pool and open issues.",
      },
    ],
  },
  {
    title: "Insight",
    // Cost is a tab of the insights page now, not a sibling destination.
    items: [
      {
        to: "/insights",
        label: "Insights",
        Icon: LineChart,
        exp: "insights",
        blurb: "Throughput trends and the cost breakdown.",
      },
    ],
  },
  {
    title: "Knowledge",
    items: [
      {
        to: "/notes",
        label: "Notes",
        Icon: FileText,
        exp: "notes",
        blurb: "Every branch note bn is keeping, rendered.",
      },
      {
        to: "/skills",
        label: "Skills",
        Icon: Zap,
        exp: "skills",
        blurb: "The skill library, editable in place and synced to the harnesses.",
      },
      {
        to: "/agents",
        label: "Agents",
        Icon: Bot,
        exp: "agents",
        blurb:
          "The Markdown agent roster — what each agent is, when it fires, and its source, read-only.",
      },
      {
        to: "/handoff",
        label: "Handoff",
        Icon: ArrowLeftRight,
        exp: "handoff",
        blurb: "A branch's resume packet — what the next session needs to know.",
      },
      {
        to: "/research",
        label: "Research",
        Icon: Microscope,
        exp: "research",
        blurb: "Findings and investigations captured by the scout agents.",
      },
    ],
  },
];

/** Only the groups that hold at least one gated tab, with only those tabs — what `/settings`
 * renders as switches. Ungated destinations (Today, Cron) have nothing to decide, so they are
 * omitted rather than shown as a switch that cannot move. */
export const EXPERIMENTAL_GROUPS: NavGroup[] = NAV_GROUPS.map((g) => ({
  ...g,
  items: g.items.filter((i) => i.exp),
})).filter((g) => g.items.length > 0);

/** Every `?experimental=` key the shell knows about. */
export const EXPERIMENTAL_KEYS: string[] = EXPERIMENTAL_GROUPS.flatMap((g) =>
  g.items.map((i) => i.exp as string),
);
