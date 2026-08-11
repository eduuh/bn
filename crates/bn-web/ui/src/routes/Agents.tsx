import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { AlertTriangle, Bot, Plus, Sparkles } from "lucide-react";
import { agentDefsQuery } from "../api/client";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
import { Skeleton } from "../components/ui/skeleton";
import { AgentEditor } from "../components/agents/AgentEditor";
import { AgentSidebar } from "../components/agents/AgentSidebar";
import { NewAgentForm } from "../components/agents/NewAgentForm";
import { PageHead } from "../components/PageHead";
import { ToastStack } from "../components/ToastStack";

/**
 * `/agents` — the Markdown agent roster: every agent defined in an `agents/*.md` across the branch
 * / repo / global tiers, in a sidebar, with the selected one's source open beside it and editable.
 *
 * This is the *definition* roster, not the live fleet (`/agents/:id`, keyed by harness session id)
 * and not the triggerable catalog (`/api/agents/catalog`, which drops specialists because nothing
 * can fire them). Here a specialist is listed like anything else — you can read and edit what it
 * says even though it only ever compiles into a native harness agent.
 *
 * Writes are validated server-side and never client-side, for a reason worth stating once: one
 * unparseable file fails `agents::discover` for the *entire* catalog, which would stop the
 * scheduler dispatching anything at all. The server parses the exact bytes it is about to write, so
 * a bad edit is a rejected request rather than a roster that breaks on the next tick.
 */
export function Agents() {
  const roster = useQuery(agentDefsQuery());
  const [selected, setSelected] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [creating, setCreating] = useState(false);

  // Memoised so the identity is stable across renders — an inline `?? []` would be a fresh array
  // every time and re-run the sidebar's filter on every keystroke elsewhere on the page.
  const all = useMemo(() => roster.data?.agents ?? [], [roster.data]);
  const scheduled = all.filter((a) => a.schedule).length;

  return (
    <div className="space-y-5 pb-8">
      <PageHead
        title="Agents"
        sub={
          roster.isLoading
            ? "loading…"
            : `${all.length} defined · ${scheduled} scheduled · create, edit & delete`
        }
        right={
          !creating && (
            <Button onClick={() => setCreating(true)}>
              <Plus className="h-4 w-4" /> New agent
            </Button>
          )
        }
      />

      {creating && (
        <NewAgentForm
          onCancel={() => setCreating(false)}
          onDone={(name) => {
            setCreating(false);
            setSelected(name);
          }}
        />
      )}

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

      {!roster.isLoading && !roster.isError && all.length === 0 && !creating && (
        <Card className="flex flex-col items-center gap-2 px-6 py-12 text-center">
          <Bot className="h-8 w-8 text-muted-foreground" />
          <h3 className="text-lg font-semibold">No agents</h3>
          <p className="max-w-sm text-sm text-muted-foreground">
            No Markdown agents defined in any tier. They live in{" "}
            <code className="font-mono">agents/*.md</code>.
          </p>
          <Button className="mt-2" onClick={() => setCreating(true)}>
            <Plus className="h-4 w-4" /> New agent
          </Button>
        </Card>
      )}

      {/* Both grid tracks are `minmax(0,…)`: a grid child defaults to `min-width:auto`, so the long
          frontmatter lines in the source pane would widen the column and scroll the whole page
          sideways instead of scrolling inside their own box. */}
      {all.length > 0 && (
        <div className="grid grid-cols-1 items-start gap-4 md:grid-cols-[minmax(0,20rem)_minmax(0,1fr)]">
          <AgentSidebar
            agents={all}
            selected={selected}
            filter={filter}
            onFilter={setFilter}
            onSelect={setSelected}
          />
          <Card className="p-4">
            {selected ? (
              <AgentEditor name={selected} onDeleted={() => setSelected(null)} />
            ) : (
              <div className="flex flex-col items-center gap-2 px-6 py-12 text-center">
                <Sparkles className="h-8 w-8 text-muted-foreground" />
                <p className="max-w-sm text-sm text-muted-foreground">
                  Pick an agent to read and edit its Markdown definition.
                </p>
              </div>
            )}
          </Card>
        </div>
      )}

      <ToastStack />
    </div>
  );
}
