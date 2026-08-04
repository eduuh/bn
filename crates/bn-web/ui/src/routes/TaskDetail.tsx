import { Link, useParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { AlertTriangle, ArrowLeft, Loader2 } from "lucide-react";
import { FetchError, taskQuery } from "../api/client";
import type { TaskAgent } from "../api/types";
import { PageHead } from "../components/PageHead";
import MarkdownBody from "../components/MarkdownBody";
import { Badge } from "../components/ui/badge";
import { Card } from "../components/ui/card";
import { Skeleton } from "../components/ui/skeleton";

/** The health tone for the agent chip — the same scale the fleet board uses. */
const HEALTH_TONE: Record<string, "ok" | "warn" | "urgent" | "muted"> = {
  active: "ok",
  starting: "muted",
  idle: "warn",
  stuck: "urgent",
};

/** The agent working this task, linking through to its transcript. */
function AgentLine({ agent }: { agent: TaskAgent }) {
  return (
    <Link
      to={`/agents/${encodeURIComponent(agent.session_id)}`}
      className="inline-flex flex-wrap items-center gap-2 rounded-md border border-border bg-card px-2 py-1.5 text-sm transition-colors hover:bg-accent/40"
    >
      <Badge variant={HEALTH_TONE[agent.health] ?? "muted"}>{agent.health}</Badge>
      <span className="font-mono text-xs text-foreground">{agent.target}</span>
      <span className="text-xs text-muted-foreground">{agent.harness}</span>
      <span className="text-xs text-muted-foreground">
        idle {Math.round(agent.idle_secs / 60)}m
      </span>
    </Link>
  );
}

/**
 * `/tasks/:id` — one task, and the thing working on it.
 *
 * A task had no address before this: the store could list it and the pipeline could show it in a
 * row, but nothing could *link* to one — so the fleet's new "this agent is on t42" chip had nowhere
 * to point. The round trip closes here: fleet chip → this page → the agent chip → `/agents/:id`.
 *
 * Everything is served by one `GET /api/tasks/:id`, joined server-side, so the task and the agent
 * beside it can never be rendered out of sync with each other.
 */
export function TaskDetail() {
  const { id = "" } = useParams();
  const q = useQuery(taskQuery(id));

  const notFound = q.error instanceof FetchError && q.error.status === 404;
  const task = q.data?.task;

  return (
    <div className="space-y-4 pb-8">
      <Link
        to="/fleet"
        className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
      >
        <ArrowLeft className="h-4 w-4" /> Fleet
      </Link>

      {q.isLoading && (
        <div className="space-y-3">
          <div className="flex items-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="h-4 w-4 animate-spin" /> Loading task…
          </div>
          <Skeleton className="h-5 w-2/3" />
          <Skeleton className="h-4 w-1/3" />
        </div>
      )}

      {/* A missing task gets a real screen, not a blank one — a stale link is a normal thing to
          follow, and "nothing rendered" is indistinguishable from a broken page. */}
      {notFound && (
        <Card className="flex flex-col items-center gap-2 px-6 py-12 text-center">
          <AlertTriangle className="h-7 w-7 text-muted-foreground" />
          <h3 className="text-lg font-semibold">No task {id}</h3>
          <p className="text-sm text-muted-foreground">
            It may have been renumbered — task ids are positional handles, not identities.
          </p>
        </Card>
      )}

      {q.isError && !notFound && (
        <Card className="flex items-center gap-2 border-urgent/30 bg-urgent-muted p-4 text-sm text-urgent">
          <AlertTriangle className="h-4 w-4" />
          {(q.error as Error).message}
        </Card>
      )}

      {task && (
        <>
          <PageHead
            title={task.summary}
            sub={
              <>
                {task.id}
                {task.repo && ` · ${task.repo}`}
                {task.branch && `/${task.branch}`}
              </>
            }
          />

          <div className="flex flex-wrap items-center gap-1.5">
            <Badge variant={task.status === "done" ? "ok" : "muted"}>{task.status}</Badge>
            {task.priority && <Badge variant="warn">{task.priority}</Badge>}
            {task.stage && <Badge variant="info">{task.stage}</Badge>}
            {task.smart && (
              <Badge variant={task.smart === "ok" ? "ok" : "urgent"}>smart: {task.smart}</Badge>
            )}
            {task.day && <Badge variant="muted">{task.day}</Badge>}
            {task.est && <Badge variant="muted">est {task.est}</Badge>}
            {task.src && <Badge variant="muted">{task.src}</Badge>}
          </div>

          <section className="space-y-2">
            <h3 className="text-sm font-semibold uppercase tracking-wide text-muted-foreground">
              Working on it
            </h3>
            {q.data?.agent ? (
              <AgentLine agent={q.data.agent} />
            ) : (
              // Stated, not omitted: "no agent" and "we didn't load the agent" must not look alike.
              <p className="text-sm text-muted-foreground">
                {task.repo && task.branch
                  ? `Nothing is running on ${task.repo}/${task.branch}.`
                  : "Not bound to a worktree yet — dispatch it with --task, or run `bn task claim` from its checkout."}
              </p>
            )}
          </section>

          {q.data?.doc?.body && (
            <section className="space-y-2">
              <h3 className="text-sm font-semibold uppercase tracking-wide text-muted-foreground">
                Context
              </h3>
              <Card className="p-4">
                <MarkdownBody text={q.data.doc.body} />
              </Card>
            </section>
          )}

          {q.data?.note && task.repo && task.branch && (
            <section className="space-y-2">
              <h3 className="text-sm font-semibold uppercase tracking-wide text-muted-foreground">
                Branch note
              </h3>
              <Link
                to={`/notes?repo=${encodeURIComponent(task.repo)}&branch=${encodeURIComponent(task.branch)}`}
                className="inline-flex items-center rounded-md border border-border bg-card px-2 py-1.5 font-mono text-xs text-primary transition-colors hover:bg-accent/40"
              >
                {task.repo}/{task.branch}
              </Link>
            </section>
          )}
        </>
      )}
    </div>
  );
}
