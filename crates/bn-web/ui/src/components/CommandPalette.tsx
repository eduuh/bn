import { useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import {
  AlertTriangle,
  ArrowLeftRight,
  Ban,
  Bell,
  Bot,
  CircleDollarSign,
  CircleDot,
  ClipboardCheck,
  Clock,
  CornerDownLeft,
  FileText,
  FolderGit2,
  GitPullRequest,
  GraduationCap,
  HardDrive,
  LineChart,
  ListTodo,
  type LucideIcon,
  Microscope,
  Rocket,
  Server,
  Sparkles,
  SquareTerminal,
  Sun,
  Target,
  Workflow,
  Zap,
} from "lucide-react";
import { searchQuery } from "../api/client";
import type { ActionItem, SearchResult, SearchResultKind } from "../api/types";
import { StartAgentComposer } from "./StartAgentComposer";
import { ToastStack } from "./ToastStack";
import { Dialog, DialogContent, DialogTitle } from "./ui/dialog";
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "./ui/command";

/** Icon per result kind — the row's at-a-glance type cue. */
const KIND_ICON: Record<SearchResultKind, LucideIcon> = {
  todo: ListTodo,
  blocker: Ban,
  attention: AlertTriangle,
  "plan-review": ClipboardCheck,
  "failing-cron": Clock,
  "stale-pr": GitPullRequest,
  goal: Target,
  issue: CircleDot,
  suggestion: Sparkles,
  note: FileText,
  learning: GraduationCap,
  agent: Bot,
  repo: FolderGit2,
  cron: Clock,
  skill: Sparkles,
};

/** The trailing hint for a row — the action Enter/click will take. */
function verb(r: SearchResult): "Work on" | "Open" | "Jump" {
  if (r.action) return "Work on";
  if (r.link) return "Open";
  return "Jump";
}

/**
 * The app's own destinations, so ⌘K can answer the most common thing anyone types into a palette:
 * the name of a screen.
 *
 * `/api/search` ranks *data* — tasks, notes, agents, repos. It has no notion of the dashboard's own
 * navigation, so "cost" and "console" both dead-ended on "No results.", and names that appeared to
 * work ("today", "cron") only matched because some data row happened to contain the word. These are
 * matched client-side and rendered above the server's hits: navigation is instant and local, so it
 * should never wait on a fetch or lose to a fuzzy data match.
 *
 * `keywords` carry the synonyms the label misses — what you'd type when you don't recall the name
 * bn uses ("money" for Cost, "agents" for Fleet, "schedule" for Cron).
 */
const DESTINATIONS: { to: string; label: string; Icon: LucideIcon; keywords: string }[] = [
  { to: "/", label: "Console", Icon: SquareTerminal, keywords: "home queue index start work" },
  { to: "/fleet", label: "Fleet", Icon: Server, keywords: "agents running live sessions workers" },
  { to: "/nodes", label: "Nodes", Icon: HardDrive, keywords: "machines hosts cpu ram health" },
  { to: "/cron", label: "Cron", Icon: Clock, keywords: "jobs schedule timers automation runs" },
  { to: "/attention", label: "Attention", Icon: Bell, keywords: "alerts needs you urgent inbox" },
  { to: "/inbox", label: "Automation", Icon: Workflow, keywords: "reviews plans completed history" },
  { to: "/today", label: "Today", Icon: Sun, keywords: "daily aimed achieved standup goals" },
  { to: "/goals", label: "Goals", Icon: Target, keywords: "objectives quarter daily targets" },
  { to: "/repos", label: "Repos", Icon: FolderGit2, keywords: "repositories projects worktrees" },
  { to: "/insights", label: "Insights", Icon: LineChart, keywords: "trends metrics analytics charts" },
  { to: "/cost", label: "Cost", Icon: CircleDollarSign, keywords: "spend money usage tokens aic bill" },
  { to: "/notes", label: "Notes", Icon: FileText, keywords: "branch notes writeups docs" },
  { to: "/skills", label: "Skills", Icon: Zap, keywords: "mcp tools capabilities prompts" },
  { to: "/handoff", label: "Handoff", Icon: ArrowLeftRight, keywords: "context transfer session wrap" },
  { to: "/research", label: "Research", Icon: Microscope, keywords: "findings investigations scout" },
  { to: "/start", label: "Start an agent", Icon: Rocket, keywords: "dispatch new run queue daemon" },
];

/** Destinations matching `q` — label first, then keyword hits, capped so they never bury the data. */
function matchDestinations(q: string): typeof DESTINATIONS {
  const needle = q.trim().toLowerCase();
  if (!needle) return DESTINATIONS;
  const byLabel = DESTINATIONS.filter((d) => d.label.toLowerCase().includes(needle));
  const byKeyword = DESTINATIONS.filter(
    (d) => !byLabel.includes(d) && d.keywords.includes(needle),
  );
  return [...byLabel, ...byKeyword].slice(0, 6);
}

/**
 * The ⌘K global search / command palette (#565) — reachable from anywhere in the shell. It
 * debounce-queries `GET /api/search` and renders one ranked, typed list across everything bn
 * tracks (tasks, blockers, attention, activities, notes, learnings, agents, repos, crons, goals,
 * and best-effort GitHub issues). Selecting a result acts on it: results carrying an `action`
 * open the reused [`StartAgentComposer`] pre-filled ("work on this task/issue"); an external
 * `link` opens in a new tab; otherwise the in-app `route` is navigated. cmdk's own fuzzy filter is
 * off (`shouldFilter={false}`) — the ranking is the server's.
 */
export function CommandPalette({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const navigate = useNavigate();
  const [raw, setRaw] = useState("");
  const [debounced, setDebounced] = useState("");
  const [composing, setComposing] = useState<ActionItem | null>(null);

  // The box updates instantly; the endpoint sees a debounced value so we don't fire per keystroke.
  useEffect(() => {
    const t = setTimeout(() => setDebounced(raw), 150);
    return () => clearTimeout(t);
  }, [raw]);

  // A fresh search each time the palette opens.
  useEffect(() => {
    if (open) {
      setRaw("");
      setDebounced("");
    }
  }, [open]);

  const search = useQuery(searchQuery(debounced));
  const results = search.data?.results ?? [];
  const typed = debounced.trim().length > 0;
  // Matched against the *raw* box, not the debounced value: navigation is local, so it should feel
  // instant rather than trailing the 150ms the data search needs. The group heading below reads
  // `raw` for the same reason — heading and list must not disagree while the search debounces.
  const destinations = matchDestinations(raw);

  const run = (r: SearchResult) => {
    if (r.action) {
      // "Work on this" — hand the embedded action to the same composer the command center uses.
      setComposing(r.action);
      onOpenChange(false);
      return;
    }
    onOpenChange(false);
    if (r.link) {
      window.open(r.link, "_blank", "noopener,noreferrer");
      return;
    }
    if (r.route) navigate(r.route);
  };

  return (
    <>
      <Dialog open={open} onOpenChange={onOpenChange}>
        <DialogContent
          hideClose
          className="overflow-hidden p-0 shadow-lg sm:max-w-xl"
          aria-label="Global search"
        >
          <DialogTitle className="sr-only">Global search</DialogTitle>
          <Command
            shouldFilter={false}
            className="[&_[cmdk-group-heading]]:px-2 [&_[cmdk-group-heading]]:font-medium [&_[cmdk-group-heading]]:text-muted-foreground [&_[cmdk-input-wrapper]_svg]:h-5 [&_[cmdk-input-wrapper]_svg]:w-5 [&_[cmdk-input]]:h-12 [&_[cmdk-item]]:px-2 [&_[cmdk-item]]:py-2.5 [&_[cmdk-item]_svg]:h-5 [&_[cmdk-item]_svg]:w-5"
          >
            <CommandInput
              value={raw}
              onValueChange={setRaw}
              placeholder="Search tasks, issues, notes, agents, repos…"
            />
            <CommandList className="max-h-[min(70vh,28rem)]">
              {destinations.length > 0 && (
                <CommandGroup heading={raw.trim() ? "Go to" : "Destinations"}>
                  {destinations.map((d) => (
                    <CommandItem
                      key={d.to}
                      value={`goto:${d.to}`}
                      onSelect={() => {
                        onOpenChange(false);
                        navigate(d.to);
                      }}
                      className="gap-3"
                    >
                      <d.Icon className="h-4 w-4 shrink-0 text-muted-foreground" aria-hidden="true" />
                      <div className="min-w-0 flex-1">
                        <div className="truncate font-medium text-foreground">{d.label}</div>
                        <div className="truncate font-mono text-xs text-muted-foreground">{d.to}</div>
                      </div>
                      <span className="ml-auto inline-flex shrink-0 items-center gap-1 text-xs text-muted-foreground">
                        <CornerDownLeft className="h-3.5 w-3.5" />
                        Go
                      </span>
                    </CommandItem>
                  ))}
                </CommandGroup>
              )}
              {!typed && (
                <div className="px-4 py-6 text-center text-sm text-muted-foreground">
                  …or type to search across everything bn tracks — then act from the result.
                </div>
              )}
              {typed && !search.isFetching && results.length === 0 && destinations.length === 0 && (
                <CommandEmpty>No results.</CommandEmpty>
              )}
              {results.length > 0 && (
                <CommandGroup
                  heading={`${results.length} result${results.length === 1 ? "" : "s"}`}
                >
                  {results.map((r) => {
                    const Icon = KIND_ICON[r.type] ?? FileText;
                    const where = r.repo
                      ? `${r.repo}${r.branch ? `/${r.branch}` : ""}`
                      : null;
                    const action = verb(r);
                    return (
                      <CommandItem
                        key={r.id}
                        value={r.id}
                        onSelect={() => run(r)}
                        className="gap-3"
                      >
                        <Icon className="h-4 w-4 shrink-0 text-muted-foreground" aria-hidden="true" />
                        <div className="min-w-0 flex-1">
                          <div className="truncate font-medium text-foreground">{r.title}</div>
                          <div className="truncate text-xs text-muted-foreground">
                            <span className="uppercase tracking-wide">{r.type}</span>
                            {where && <span className="font-mono"> · {where}</span>}
                            {r.subtitle && <span> · {r.subtitle}</span>}
                          </div>
                        </div>
                        <span className="ml-auto inline-flex shrink-0 items-center gap-1 text-xs text-muted-foreground">
                          {action === "Work on" ? (
                            <Rocket className="h-3.5 w-3.5" />
                          ) : (
                            <CornerDownLeft className="h-3.5 w-3.5" />
                          )}
                          {action}
                        </span>
                      </CommandItem>
                    );
                  })}
                </CommandGroup>
              )}
            </CommandList>
          </Command>
        </DialogContent>
      </Dialog>
      {composing && (
        <StartAgentComposer item={composing} onClose={() => setComposing(null)} />
      )}
      <ToastStack />
    </>
  );
}
