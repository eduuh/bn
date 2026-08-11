import { useMemo } from "react";
import { Clock, Zap } from "lucide-react";
import type { AgentDefRow } from "../../api/types";
import { Input } from "../ui/input";
import { cn } from "../../lib/utils";
import { kindBadge, trigger } from "./agentMeta";

function AgentRow({
  agent,
  selected,
  onSelect,
}: {
  agent: AgentDefRow;
  selected: boolean;
  onSelect: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onSelect}
      aria-current={selected ? "true" : undefined}
      className={cn(
        "flex w-full flex-col gap-1 rounded-lg border px-3 py-2 text-left transition-colors",
        selected
          ? "border-primary/40 bg-primary/10"
          : "border-border bg-card hover:border-border/80 hover:bg-accent/40",
        // Nothing can fire a specialist, so it should read as quieter than the rest of the roster.
        agent.specialist && !selected && "opacity-70",
      )}
    >
      <div className="flex items-center gap-2">
        <span className="min-w-0 flex-1 truncate font-mono text-sm text-foreground">
          {agent.name}
        </span>
        {kindBadge(agent)}
      </div>
      <div className="flex items-center gap-1.5 font-mono text-[11px] text-muted-foreground">
        {agent.schedule ? (
          <Clock className="h-3 w-3 shrink-0" />
        ) : (
          <Zap className="h-3 w-3 shrink-0" />
        )}
        <span className="truncate">{trigger(agent)}</span>
      </div>
    </button>
  );
}

/** Tier order: the one that wins first, first. `agent_dirs` folds branch over repo over global, so
 * a reader scanning top-down is reading them in precedence order — which is the only order in
 * which two agents of the same name make sense. */
const SCOPE_ORDER = ["branch", "repo", "global"];

function scopeRank(scope: string): number {
  const i = SCOPE_ORDER.indexOf(scope);
  return i === -1 ? SCOPE_ORDER.length : i;
}

/**
 * The roster sidebar: filter, then every definition grouped by the tier it was discovered in.
 *
 * Grouped rather than flat because the tier is what answers "why are there two of these, and which
 * one is live?" — a branch `agents/scout.md` shadows the shipped one, and a flat alphabetical list
 * puts those two rows next to each other with nothing to say which is which.
 *
 * Sticky on desktop: the source pane beside it is a tall editor, and a sidebar that scrolls away
 * while you are two screens down a `.md` is a sidebar you have to scroll back up to use.
 */
export function AgentSidebar({
  agents,
  selected,
  filter,
  onFilter,
  onSelect,
}: {
  agents: AgentDefRow[];
  selected: string | null;
  filter: string;
  onFilter: (next: string) => void;
  onSelect: (name: string) => void;
}) {
  const shown = useMemo(() => {
    const q = filter.trim().toLowerCase();
    if (!q) return agents;
    return agents.filter(
      (a) =>
        a.name.toLowerCase().includes(q) ||
        (a.description ?? "").toLowerCase().includes(q) ||
        (a.harness ?? "").toLowerCase().includes(q),
    );
  }, [agents, filter]);

  // Grouped as a list of pairs rather than a Record, so the tier order is the render order and does
  // not depend on object key insertion.
  const groups = useMemo(() => {
    const by = new Map<string, AgentDefRow[]>();
    for (const a of shown) {
      const list = by.get(a.scope);
      if (list) list.push(a);
      else by.set(a.scope, [a]);
    }
    return [...by.entries()].sort((l, r) => scopeRank(l[0]) - scopeRank(r[0]));
  }, [shown]);

  return (
    <div className="md:sticky md:top-4 md:max-h-[calc(100vh-7rem)] md:overflow-y-auto md:pr-1">
      <Input
        aria-label="Filter agents"
        className="mb-3 h-8"
        placeholder="filter…"
        value={filter}
        onChange={(e) => onFilter(e.target.value)}
      />

      {groups.map(([scope, rows]) => (
        <div key={scope} className="mb-4 last:mb-0">
          <h3 className="mb-1.5 px-1 text-[0.64rem] font-semibold uppercase tracking-[0.09em] text-muted-foreground/80">
            {scope} · {rows.length}
          </h3>
          <div className="space-y-2">
            {rows.map((a) => (
              <AgentRow
                key={`${a.scope}:${a.name}`}
                agent={a}
                selected={selected === a.name}
                onSelect={() => onSelect(a.name)}
              />
            ))}
          </div>
        </div>
      ))}

      {shown.length === 0 && (
        <p className="px-1 py-6 text-center text-sm text-muted-foreground">
          No agent matches “{filter}”.
        </p>
      )}
    </div>
  );
}
