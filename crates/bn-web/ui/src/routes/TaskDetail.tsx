import { useRef, useState } from "react";
import { Link, useParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { AlertTriangle, ArrowLeft, Loader2, Pencil, Plus, Save, X } from "lucide-react";
import { FetchError, queryKeys, taskQuery } from "../api/client";
import type { TaskAgent, TaskDetailView } from "../api/types";
import { addTaskAttachments, ApiError, appendTaskDoc, editTaskDoc } from "../api/writes";
import { useAppMutation } from "../hooks/useAppMutation";
import { PageHead } from "../components/PageHead";
import MarkdownBody from "../components/MarkdownBody";
import { TaskAttachments } from "../components/TaskAttachments";
import type { PendingFile } from "../lib/capture";
import { attachmentResolver, fileToPending, MAX_FILE_BYTES, toUploads } from "../lib/capture";
import { ToastStack } from "../components/ToastStack";
import { BnFileBrowser, PlanMarkdownViewer, PlanSummaryCard } from "../components/PlanArtifacts";
import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
import { Skeleton } from "../components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "../components/ui/tabs";
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
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(doc.body);
  const [context, setContext] = useState("");
  const [conflict, setConflict] = useState(false);
  const editorRef = useRef<HTMLTextAreaElement>(null);
  const docKey = [queryKeys.task(taskId)];

  /**
   * Paste or drop a file straight into the document.
   *
   * The upload has to finish before the text can be written, because the server is what settles
   * the stored name — a second `image.png` becomes `image-2.png`, and a reference written from the
   * name the browser guessed would point at the wrong file. So: upload, then insert the name the
   * server actually used, at the cursor.
   */
  const attach = useAppMutation<{ attachments: string[] }, PendingFile[]>({
    mutationFn: (pending) => addTaskAttachments(taskId, toUploads(pending)),
    invalidates: [queryKeys.taskAttachments(taskId), queryKeys.task(taskId)],
    toast: { error: "Could not attach" },
    onSuccess: (result) => {
      const markdown = result.attachments.map((name) => `![${name}](${name})`).join("\n");
      const box = editorRef.current;
      // Insert at the caret when the editor has one, else append — a drop can land without focus.
      const at = box?.selectionStart ?? draft.length;
      setDraft((prev) => `${prev.slice(0, at)}\n${markdown}\n${prev.slice(at)}`);
    },
  });

  const takeFiles = async (files: File[]) => {
    const ok = files.filter((f) => f.size <= MAX_FILE_BYTES);
    if (!ok.length) return;
    const taken = new Set<string>();
    const pending: PendingFile[] = [];
    for (const file of ok) {
      const read = await fileToPending(file, taken);
      taken.add(read.name);
      pending.push(read);
    }
    attach.mutate(pending);
  };

  const append = useAppMutation<unknown, string>({
    mutationFn: (text) => appendTaskDoc(taskId, text),
    invalidates: docKey,
    toast: { success: "Added to Investigation", error: "Could not add context" },
    onSuccess: () => setContext(""),
  });

  const isConflict = (e: Error) => e instanceof ApiError && e.code === "doc_conflict";

  const save = useAppMutation({
    mutationFn: () => editTaskDoc(taskId, draft, doc.version),
    invalidates: docKey,
    toast: {
      success: "Saved task.md",
      // A lost race is not something to report and forget: the operator's text is kept, and the
      // reconcile on settle pulls the version that landed so they can merge and save again.
      error: (e) =>
        isConflict(e)
          ? "Someone else saved first — your text is kept; the latest is shown below"
          : `Could not save: ${e.message}`,
    },
    onSuccess: () => {
      setConflict(false);
      setEditing(false);
    },
    onError: (e) => setConflict(isConflict(e)),
  });

  if (editing) {
    return (
      <Card className="space-y-2 p-3">
        <Textarea
          ref={editorRef}
          className="min-h-[24rem] font-mono text-[13px] leading-relaxed"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          spellCheck={false}
          onPaste={(e) => {
            const pasted = Array.from(e.clipboardData.files);
            if (!pasted.length) return;
            e.preventDefault();
            void takeFiles(pasted);
          }}
          onDragOver={(e) => e.preventDefault()}
          onDrop={(e) => {
            const dropped = Array.from(e.dataTransfer.files);
            if (!dropped.length) return;
            e.preventDefault();
            void takeFiles(dropped);
          }}
        />
        <p className="text-[11px] text-muted-foreground">
          {attach.isPending
            ? "attaching…"
            : "Paste or drop a screenshot to attach it and reference it here."}
        </p>
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
        {/* Attachments are referred to by bare name in the doc (see `bn_core::task_attach`), so
            the preview has to point them at this task's attachment endpoint or every pasted
            screenshot renders as a broken image. */}
        <MarkdownBody text={doc.body} resolveAsset={attachmentResolver(taskId)} />
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
        to="/queue"
        className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
      >
        <ArrowLeft className="h-4 w-4" /> Queue
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

          <section className="space-y-2">
            <h3 className="text-sm font-semibold uppercase tracking-wide text-muted-foreground">
              Attachments
            </h3>
            <TaskAttachments taskId={task.id} />
          </section>

          {q.data?.note && task.repo && task.branch && (
            <section className="space-y-2">
              <h3 className="text-sm font-semibold uppercase tracking-wide text-muted-foreground">
                Branch bn files
              </h3>
              <Link
                to={`/notes?repo=${encodeURIComponent(task.repo)}&branch=${encodeURIComponent(task.branch)}`}
                className="inline-flex items-center rounded-md border border-border bg-card px-2 py-1.5 font-mono text-xs text-primary transition-colors hover:bg-accent/40"
              >
                {task.repo}/{task.branch}
              </Link>
              <PlanSummaryCard repo={task.repo} branch={task.branch} />
              <Tabs defaultValue="plan">
                <TabsList>
                  <TabsTrigger value="plan">Plan</TabsTrigger>
                  <TabsTrigger value="files">Files</TabsTrigger>
                </TabsList>
                <TabsContent value="plan">
                  <PlanMarkdownViewer repo={task.repo} branch={task.branch} />
                </TabsContent>
                <TabsContent value="files">
                  <BnFileBrowser repo={task.repo} branch={task.branch} />
                </TabsContent>
              </Tabs>
            </section>
          )}
        </>
      )}
      <ToastStack />
    </div>
  );
}
