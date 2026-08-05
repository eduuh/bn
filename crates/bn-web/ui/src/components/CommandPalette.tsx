import { useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import {
  AlertTriangle,
  Ban,
  Bot,
  CircleDot,
  ClipboardCheck,
  Clock,
  CornerDownLeft,
  FileText,
  FolderGit2,
  GitPullRequest,
  GraduationCap,
  ListTodo,
  type LucideIcon,
  Rocket,
  Sparkles,
  Target,
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
              {!typed && (
                <div className="px-4 py-10 text-center text-sm text-muted-foreground">
                  Type to search across everything bn tracks — then act from the result.
                </div>
              )}
              {typed && !search.isFetching && results.length === 0 && (
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
