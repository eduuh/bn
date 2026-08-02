import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, Plus, Rocket } from "lucide-react";
import { queryKeys, reposQuery } from "../../api/client";
import { apiDispatch, apiQueueAdd, attnAction } from "../../api/writes";
import { pushToast } from "../../lib/toast";
import type { WorkItem } from "../../lib/console";
import { PlanReviewPanel } from "../PlanReviewPanel";
import { SteerPanel } from "../SteerPanel";
import { LandButton } from "../LandButton";
import { Button } from "../ui/button";
import { Card } from "../ui/card";
import { Textarea } from "../ui/textarea";
import { Input } from "../ui/input";
import { Label } from "../ui/label";

/** Pull a string field out of an action's loosely-typed `payload`. */
function payloadStr(item: WorkItem, key: string): string {
  const v = item.action?.payload?.[key];
  return typeof v === "string" ? v : "";
}

/**
 * Inline dispatch — the composer, **not** a modal. On a second screen the whole point is to keep
 * the queue and the thing you're reading on screen while you write the instruction, so the prompt
 * lives in the act column beside them rather than covering both.
 */
function DispatchPanel({ item }: { item: WorkItem }) {
  const action = item.action!;
  const qc = useQueryClient();
  const [prompt, setPrompt] = useState(action.suggested_prompt);
  const [repo, setRepo] = useState(payloadStr(item, "repo") || action.repo || "");
  const repos = useQuery(reposQuery());
  // Managed repos first, then the rest by recent activity — the order you'd actually reach for.
  const knownRepos = (repos.data?.repos ?? [])
    .slice()
    .sort((a, b) => Number(b.managed) - Number(a.managed) || b.last_activity_ts - a.last_activity_ts)
    .map((r) => r.repo);

  const invalidate = () => {
    void qc.invalidateQueries({ queryKey: queryKeys.actions });
    void qc.invalidateQueries({ queryKey: queryKeys.fleet });
    void qc.invalidateQueries({ queryKey: queryKeys.orchestrate });
  };

  const dispatch = useMutation({
    mutationFn: () =>
      apiDispatch({
        repo: repo.trim(),
        item: prompt.trim(),
        harness: "claude",
        kind: action.type === "goal" ? "scout" : "ship",
      }),
    onSuccess: () => {
      pushToast("Dispatched an agent", "success");
      invalidate();
    },
    onError: (e) => pushToast(e instanceof Error ? e.message : "could not dispatch", "error"),
  });

  const queue = useMutation({
    mutationFn: () => apiQueueAdd({ repo: repo.trim() || undefined, items: [prompt.trim()] }),
    onSuccess: () => {
      pushToast("Queued for the next tick", "success");
      invalidate();
    },
    onError: (e) => pushToast(e instanceof Error ? e.message : "could not queue", "error"),
  });

  const busy = dispatch.isPending || queue.isPending;
  const hasPrompt = prompt.trim().length > 0;

  return (
    <Card className="space-y-3 p-3.5">
      <h3 className="text-[0.66rem] font-semibold uppercase tracking-[0.09em] text-muted-foreground">
        Send an agent
      </h3>

      {/* A datalist, not a bare text box: the repo is nearly always one you already run, so it
          should be a pick rather than something you recall and spell. Free text still works for a
          repo bn hasn't seen yet. */}
      <div className="space-y-1.5">
        <Label htmlFor="act-repo" className="text-xs text-muted-foreground">
          Repo
        </Label>
        <Input
          id="act-repo"
          list="act-repo-options"
          value={repo}
          onChange={(e) => setRepo(e.target.value)}
          placeholder="pick a repo"
          className="h-8 font-mono text-xs"
        />
        <datalist id="act-repo-options">
          {knownRepos.map((r) => (
            <option key={r} value={r} />
          ))}
        </datalist>
      </div>

      <div className="space-y-1.5">
        <Label htmlFor="act-prompt" className="text-xs text-muted-foreground">
          Instruction
        </Label>
        <Textarea
          id="act-prompt"
          value={prompt}
          onChange={(e) => setPrompt(e.target.value)}
          rows={7}
          className="min-h-[9rem] font-mono text-[0.72rem] leading-relaxed"
          placeholder="What should the agent do?"
        />
      </div>

      {/* One primary action. "Queue instead" stays available as a quiet link rather than a second
          equal-weight button — choosing between dispatch and queue is a decision the operator
          shouldn't have to make on every item. */}
      <div className="space-y-1.5">
        <Button
          className="w-full"
          disabled={busy || !hasPrompt || !repo.trim()}
          onClick={() => dispatch.mutate()}
          title="dispatch an agent now"
        >
          <Rocket className="h-4 w-4" /> Send agent
        </Button>
        <button
          type="button"
          disabled={busy || !hasPrompt}
          onClick={() => queue.mutate()}
          className="w-full text-center text-[0.7rem] text-muted-foreground underline-offset-2 hover:text-foreground hover:underline disabled:opacity-50"
          title="add to the orchestrate queue for the next tick"
        >
          or queue it for the next tick
        </button>
      </div>
    </Card>
  );
}

/** Resolve an attention item in place — ack it, or push it onto the branch note's todo list. An
 * attention row that just needs acknowledging never warranted spawning an agent. */
function AttnPanel({ id }: { id: string }) {
  const qc = useQueryClient();
  const invalidate = () => {
    void qc.invalidateQueries({ queryKey: queryKeys.actions });
    void qc.invalidateQueries({ queryKey: queryKeys.attn });
  };
  const act = useMutation({
    mutationFn: (verb: "ack" | "plan") => attnAction(id, verb),
    onSuccess: (_d, verb) => {
      pushToast(verb === "ack" ? "Acknowledged" : "Added to the branch note", "success");
      invalidate();
    },
    onError: (e) => pushToast(e instanceof Error ? e.message : "failed", "error"),
  });

  return (
    <Card className="space-y-2.5 p-3.5">
      <h3 className="text-[0.66rem] font-semibold uppercase tracking-[0.09em] text-muted-foreground">
        Resolve
      </h3>
      <p className="text-xs leading-relaxed text-muted-foreground">
        Clear it without spawning an agent, or file it as a todo on the branch note.
      </p>
      <div className="flex flex-col gap-2">
        <Button
          size="sm"
          variant="secondary"
          disabled={act.isPending}
          onClick={() => act.mutate("ack")}
        >
          <Check className="h-4 w-4" /> Acknowledge
        </Button>
        <Button
          size="sm"
          variant="ghost"
          disabled={act.isPending}
          onClick={() => act.mutate("plan")}
          className="text-muted-foreground"
        >
          <Plus className="h-4 w-4" /> File as todo
        </Button>
      </div>
    </Card>
  );
}

/**
 * The console's right column: **only the decisions available for the selected item**. This is the
 * flow fix that matters most — the old list offered the same "Start agent" button on every row,
 * including plan reviews, where launching a second agent is precisely the wrong move. Here a
 * review offers approve/request-changes/reject, a live agent offers steer/relaunch/kill/land, and
 * only genuinely un-started work offers dispatch.
 */
export function ActPane({ item }: { item?: WorkItem }) {
  if (!item) {
    return (
      <div className="px-4 py-6 text-xs leading-relaxed text-muted-foreground/60">
        Select something in the queue to see what you can do about it.
      </div>
    );
  }

  // A live agent: decide its plan if it's paused on one, then steer / land.
  if (item.agent) {
    const a = item.agent;
    return (
      <div className="space-y-3 p-3">
        <PlanReviewPanel repo={a.repo} branch={a.branch} />
        <SteerPanel target={a.target} />
        <LandButton repo={a.repo} />
      </div>
    );
  }

  const action = item.action!;
  const attnId = payloadStr(item, "attn_id");

  return (
    <div className="space-y-3 p-3">
      {/* A pending plan review is a decision, not a dispatch. */}
      {action.type === "plan-review" && action.repo && action.branch && (
        <PlanReviewPanel repo={action.repo} branch={action.branch} />
      )}

      {action.type === "stale-pr" && action.repo && <LandButton repo={action.repo} />}

      {attnId && <AttnPanel id={attnId} />}

      <DispatchPanel item={item} />
    </div>
  );
}
