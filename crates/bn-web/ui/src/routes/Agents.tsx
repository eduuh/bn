import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { AlertTriangle, Bot, Clock, Sparkles, Zap } from "lucide-react";
import { agentDefQuery, agentDefsQuery } from "../api/client";
import type { AgentDefRow } from "../api/types";
import { Badge } from "../components/ui/badge";
import { Card } from "../components/ui/card";
import { Input } from "../components/ui/input";
import { Skeleton } from "../components/ui/skeleton";
import { cn } from "../lib/utils";
import { PageHead } from "../components/PageHead";

/** When this agent runs, in the fewest words that are still true. */
function trigger(a: AgentDefRow): string {
  if (a.specialist) return "installed";
  if (a.schedule) return a.schedule;
  if (a.startup) return "startup";
  return "on demand";
}

/** The chip that says what *kind* of thing this is — the one distinction that changes how you read
 * everything else on the row. A specialist never runs; a staff agent produces briefing cards. */
function kindBadge(a: AgentDefRow) {
  if (a.specialist) return <Badge variant="muted">specialist</Badge>;
  if (a.kind === "staff") return <Badge variant="info">staff</Badge>;
  if (a.kind === "hosted") return <Badge variant="warn">hosted</Badge>;
  return <Badge variant="outline">command</Badge>;
}

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
      className={cn(
        "flex w-full flex-col gap-1 rounded-lg border px-3 py-2 text-left transition-colors",
        selected
          ? "border-primary/40 bg-primary/10"
          : "border-border bg-card hover:border-border/80 hover:bg-accent/40",
        // Nothing can fire a specialist, so it should read as quieter than the rest of the roster.
        agent.specialist && "opacity-70",
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

/** One labelled fact in the detail pane's metadata grid. Renders nothing when there is no value —
 * an empty "Model —" row is noise, not information. */
function Fact({ label, value }: { label: string; value?: string | null }) {
  if (!value) return null;
  return (
    <div className="min-w-0">
      <div className="text-[11px] uppercase tracking-wide text-muted-foreground/70">{label}</div>
      <div className="truncate font-mono text-[12.5px] text-foreground" title={value}>
        {value}
      </div>
    </div>
  );
}

/** The source pane: the agent's metadata, then its `.md` verbatim — frontmatter included, because
 * the frontmatter *is* half the definition (schedule, harness, model, declared arguments). */
function AgentSource({ name }: { name: string }) {
  const detail = useQuery(agentDefQuery(name));

  if (detail.isLoading) {
    return (
      <div className="space-y-3" aria-hidden="true">
        <Skeleton className="h-5 w-40" />
        <Skeleton className="h-3 w-2/3" />
        <Skeleton className="h-64 w-full" />
      </div>
    );
  }
  if (detail.isError) {
    return (
      <Card className="flex items-center gap-2 border-urgent/30 bg-urgent-muted p-4 text-sm text-urgent">
        <AlertTriangle className="h-4 w-4" />
        Could not load “{name}”.
      </Card>
    );
  }

  const a = detail.data?.agent;
  const source = detail.data?.source ?? "";
  return (
    <div className="space-y-4">
      <div className="space-y-2">
        <div className="flex flex-wrap items-center gap-2">
          <h3 className="font-mono text-sm font-semibold text-foreground">{name}</h3>
          {a && kindBadge(a)}
          {a && <Badge variant="muted">{a.scope}</Badge>}
        </div>
        {a?.description && <p className="text-sm text-muted-foreground">{a.description}</p>}
      </div>

      {a && (
        <div className="grid grid-cols-2 gap-x-4 gap-y-3 sm:grid-cols-3">
          <Fact label="Trigger" value={trigger(a)} />
          <Fact label="Harness" value={a.harness} />
          <Fact label="Model" value={a.model} />
          <Fact label="Runs" value={a.run} />
          <Fact label="Gathers" value={a.gather} />
          <Fact label="Repo" value={a.repo} />
          <Fact label="Skills" value={a.skills.length ? a.skills.join(", ") : null} />
          <Fact label="MCPs" value={a.mcps.length ? a.mcps.join(", ") : null} />
          <Fact
            label="Arguments"
            value={
              a.arguments.length
                ? a.arguments.map((x) => (x.required ? x.name : `${x.name}?`)).join(", ")
                : null
            }
          />
        </div>
      )}

      {a && (
        <div className="font-mono text-[11px] text-muted-foreground/70 break-all" title={a.path}>
          {a.path}
        </div>
      )}

      {/* Wide lines scroll inside the block rather than pushing the page sideways. */}
      <pre className="max-h-[32rem] overflow-auto rounded-lg border border-border bg-muted/40 p-3 font-mono text-[12.5px] leading-relaxed text-foreground">
        {source}
      </pre>
    </div>
  );
}

/**
 * `/agents` — the Markdown agent roster: every agent defined in a `config/agents/*.md` across the
 * branch / repo / global tiers, and the source of each one.
 *
 * Read-only on purpose. This is the *definition* roster, not the live fleet (`/agents/:id`, keyed
 * by harness session id) and not the triggerable catalog (`/api/agents/catalog`, which drops
 * specialists because nothing can fire them). Here a specialist is listed like anything else — you
 * can read what it says even though it only ever compiles into a native harness agent.
 */
export function Agents() {
  const roster = useQuery(agentDefsQuery());
  const [selected, setSelected] = useState<string | null>(null);
  const [filter, setFilter] = useState("");

  // Memoised so the identity is stable across renders — an inline `?? []` would be a fresh array
  // every time and re-run the filter below on every keystroke elsewhere on the page.
  const all = useMemo(() => roster.data?.agents ?? [], [roster.data]);
  const shown = useMemo(() => {
    const q = filter.trim().toLowerCase();
    if (!q) return all;
    return all.filter(
      (a) =>
        a.name.toLowerCase().includes(q) ||
        (a.description ?? "").toLowerCase().includes(q) ||
        (a.harness ?? "").toLowerCase().includes(q),
    );
  }, [all, filter]);

  const scheduled = all.filter((a) => a.schedule).length;

  return (
    <div className="space-y-5 pb-8">
      <PageHead
        title="Agents"
        sub={
          roster.isLoading
            ? "loading…"
            : `${all.length} defined · ${scheduled} scheduled · read-only`
        }
        right={
          all.length > 0 && (
            <Input
              aria-label="Filter agents"
              className="h-8 w-48"
              placeholder="filter…"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
            />
          )
        }
      />

      {roster.isLoading && (
        <div className="space-y-2" aria-hidden="true">
          {[0, 1, 2, 3].map((i) => (
            <Skeleton key={i} className="h-12 w-full" />
          ))}
        </div>
      )}

      {roster.isError && (
        <Card className="flex items-center gap-2 border-urgent/30 bg-urgent-muted p-4 text-sm text-urgent">
          <AlertTriangle className="h-4 w-4" />
          Could not load the agent roster.
        </Card>
      )}

      {!roster.isLoading && !roster.isError && all.length === 0 && (
        <Card className="flex flex-col items-center gap-2 px-6 py-12 text-center">
          <Bot className="h-8 w-8 text-muted-foreground" />
          <h3 className="text-lg font-semibold">No agents</h3>
          <p className="max-w-sm text-sm text-muted-foreground">
            No Markdown agents defined in any tier. They live in{" "}
            <code className="font-mono">config/agents/*.md</code>.
          </p>
        </Card>
      )}

      {/* Both grid tracks below are `minmax(0,…)`: a grid child defaults to `min-width:auto`, so
          the long frontmatter lines in the source block would widen the column and scroll the whole
          page sideways instead of scrolling inside their own `<pre>`. */}
      {all.length > 0 && (
        <div className="grid grid-cols-1 gap-4 md:grid-cols-[minmax(0,20rem)_minmax(0,1fr)]">
          <div className="space-y-2">
            {shown.map((a) => (
              <AgentRow
                key={`${a.scope}:${a.name}`}
                agent={a}
                selected={selected === a.name}
                onSelect={() => setSelected(a.name)}
              />
            ))}
            {shown.length === 0 && (
              <p className="px-1 py-6 text-center text-sm text-muted-foreground">
                No agent matches “{filter}”.
              </p>
            )}
          </div>
          <Card className="p-4">
            {selected ? (
              <AgentSource name={selected} />
            ) : (
              <div className="flex flex-col items-center gap-2 px-6 py-12 text-center">
                <Sparkles className="h-8 w-8 text-muted-foreground" />
                <p className="max-w-sm text-sm text-muted-foreground">
                  Pick an agent to read its definition and Markdown source.
                </p>
              </div>
            )}
          </Card>
        </div>
      )}
    </div>
  );
}
