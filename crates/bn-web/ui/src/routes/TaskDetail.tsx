import { useState } from "react";
import { Link, useParams } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, ArrowLeft, Loader2, Pencil, Plus, Save, X } from "lucide-react";
import { FetchError, queryKeys, taskQuery } from "../api/client";
import type { TaskAgent, TaskDetailView } from "../api/types";
import { ApiError, appendTaskDoc, editTaskDoc } from "../api/writes";
import { pushToast } from "../lib/toast";
import { PageHead } from "../components/PageHead";
import MarkdownBody from "../components/MarkdownBody";
import { ToastStack } from "../components/ToastStack";
import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
import { Skeleton } from "../components/ui/skeleton";
import { Textarea } from "../components/ui/textarea";

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
 * The task's `task.md`, read and edited from the browser (task t1's Done-When list).
 *
 * Two writes, two temperaments. **Add context** appends under `## Investigation` — additive, so an
 * agent's append and this box never fight over the file. **Edit** replaces the whole document,
 * guarded by the version it was read at. A lost edit comes back as a `409`: the text stays in the
 * box and the version that landed is shown, so nothing a session typed is ever silently discarded.
 */
function DocEditor({ taskId, doc }: { taskId: string; doc: NonNullable<TaskDetailView["doc"]> }) {
  const qc = useQueryClient();
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(doc.body);
  const [context, setContext] = useState("");
  const [conflict, setConflict] = useState(false);
  const refetch = () => void qc.invalidateQueries({ queryKey: queryKeys.task(taskId) });

  const append = useMutation({
    mutationFn: (text: string) => appendTaskDoc(taskId, text),
    onSuccess: () => {
      pushToast("Added to Investigation", "success");
      setContext("");
      refetch();
    },
    onError: (e) => pushToast(`Could not add context: ${e.message}`, "error"),
  });

  const save = useMutation({
    mutationFn: () => editTaskDoc(taskId, draft, doc.version),
    onSuccess: () => {
      pushToast("Saved task.md", "success");
      setConflict(false);
      setEditing(false);
      refetch();
    },
    onError: (e) => {
      // A lost race is not something to report and forget: keep the operator's text, pull the
      // version that landed, and show it so they can reconcile and save again.
      if (e instanceof ApiError && e.code === "doc_conflict") {
        pushToast(
          "Someone else saved first — your text is kept; the latest is shown below",
          "error",
        );
        setConflict(true);
        refetch();
      } else {
        pushToast(`Could not save: ${e.message}`, "error");
      }
    },
  });

  if (editing) {
    return (
      <Card className="space-y-2 p-3">
        <Textarea
          className="min-h-[24rem] font-mono text-[13px] leading-relaxed"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          spellCheck={false}
        />
        <div className="flex flex-wrap items-center gap-2">
          <Button size="sm" disabled={save.isPending} onClick={() => save.mutate()}>
            {save.isPending ? (
              <Loader2 className="h-4 w-4 animate-spin" />
            ) : (
              <Save className="h-4 w-4" />
            )}{" "}
            Save
          </Button>
          <Button
            size="sm"
            variant="secondary"
            onClick={() => {
              setDraft(doc.body);
              setConflict(false);
              setEditing(false);
            }}
          >
            <X className="h-4 w-4" /> Cancel
          </Button>
          <span className="ml-auto font-mono text-[11px] text-muted-foreground/70">
            guarded by version {doc.version.slice(0, 8)}
          </span>
        </div>
        {conflict && (
          <div className="space-y-1 rounded-md border border-warn/40 bg-warn-muted p-2 text-xs">
            <div className="flex items-center justify-between gap-2">
              <span className="font-medium text-warn">
                The doc changed under you — your text is kept above. What landed:
              </span>
              <Button
                size="sm"
                variant="secondary"
                onClick={() => {
                  setDraft(doc.body);
                  setConflict(false);
                }}
              >
                Load latest
              </Button>
            </div>
            <pre className="max-h-48 overflow-auto whitespace-pre-wrap font-mono text-muted-foreground">
              {doc.body}
            </pre>
          </div>
        )}
      </Card>
    );
  }

  return (
    <div className="space-y-3">
      <Card className="p-4">
        <MarkdownBody text={doc.body} />
      </Card>
      <div className="flex flex-col gap-2 sm:flex-row sm:items-start">
        <Textarea
          className="min-h-[3.5rem] flex-1 font-mono text-[13px]"
          placeholder="Add context — appends under ## Investigation"
          value={context}
          onChange={(e) => setContext(e.target.value)}
        />
        <div className="flex gap-2 sm:flex-col">
          <Button
            size="sm"
            variant="secondary"
            disabled={append.isPending || !context.trim()}
            onClick={() => append.mutate(context.trim())}
          >
            {append.isPending ? (
              <Loader2 className="h-4 w-4 animate-spin" />
            ) : (
              <Plus className="h-4 w-4" />
            )}{" "}
            Add context
          </Button>
          <Button
            size="sm"
            variant="ghost"
            onClick={() => {
              setDraft(doc.body);
              setEditing(true);
            }}
          >
            <Pencil className="h-4 w-4" /> Edit
          </Button>
        </div>
      </div>
    </div>
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
          {q.error.message}
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

          {q.data?.doc && (
            <section className="space-y-2">
              <h3 className="text-sm font-semibold uppercase tracking-wide text-muted-foreground">
                Context{" "}
                <span className="font-mono normal-case text-muted-foreground/70">· task.md</span>
              </h3>
              <DocEditor taskId={task.id} doc={q.data.doc} />
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
      <ToastStack />
    </div>
  );
}
