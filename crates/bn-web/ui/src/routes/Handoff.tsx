import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { AlertTriangle, Loader2 } from "lucide-react";
import type { Components } from "react-markdown";
import { handoffQuery, notesQuery, queryKeys } from "../api/client";
import type { HandoffView as HandoffData, SummaryEntry } from "../api/types";
import { saveHandoff } from "../api/writes";
import { pushToast } from "../lib/toast";
import { ToastStack } from "../components/ToastStack";
import { MARKDOWN_PROSE } from "../components/SessionNote";
import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
import { Label } from "../components/ui/label";
import { Textarea } from "../components/ui/textarea";
import { Skeleton } from "../components/ui/skeleton";
import { cn } from "../lib/utils";

const keyOf = (b: SummaryEntry): string => `${b.repo}/${b.branch}`;

/** Links in the rendered handoff markdown open safely in a new tab. */
const MD_COMPONENTS: Components = {
  a: ({ href, children }) => (
    <a href={href} target="_blank" rel="noreferrer noopener">
      {children}
    </a>
  ),
};

/** Split a textarea into a trimmed, non-empty list — one item per line (mirrors `HandoffInput`). */
function lines(text: string): string[] {
  return text
    .split("\n")
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
}

/** The selectable branch rail — one button per branch note, shared with the Notes browser. */
function BranchRail({
  branches,
  selected,
  onSelect,
}: {
  branches: SummaryEntry[];
  selected?: SummaryEntry;
  onSelect: (k: string) => void;
}) {
  return (
    <ul className="space-y-1.5">
      {branches.map((b) => {
        const k = keyOf(b);
        const active = selected && keyOf(selected) === k;
        return (
          <li key={k}>
            <button
              type="button"
              onClick={() => onSelect(k)}
              className={cn(
                "w-full rounded-lg border px-3 py-2 text-left transition-colors",
                active
                  ? "border-primary/40 bg-primary/10"
                  : "border-border bg-card hover:border-border/80 hover:bg-accent/40",
              )}
            >
              <div className="flex items-center justify-between gap-2">
                <span className="truncate font-mono text-sm font-medium text-foreground">
                  {b.repo}
                </span>
                {b.status && (
                  <Badge variant="muted" className="shrink-0">
                    {b.status}
                  </Badge>
                )}
              </div>
              <div className="mt-0.5 truncate font-mono text-xs text-muted-foreground">
                {b.branch}
              </div>
            </button>
          </li>
        );
      })}
    </ul>
  );
}

/**
 * `/handoff` — the per-branch **session resume packet** (#509). The forward-looking counterpart to
 * the note: `bn handoff read` rendered for the web so a session can be picked up (and updated) from
 * the dashboard. A branch rail (reusing the `/api/notes` rollup) drives selection; the right pane
 * renders the composed/written packet markdown and offers the author-supplied layers (narrative,
 * priority queue, deferred decisions, open questions) as editable fields. **Save** POSTs to
 * `/api/handoff` — an open write (no token, #526) that composes + persists `handoff.json`.
 */
export function Handoff() {
  const notes = useQuery(notesQuery());
  const branches = notes.data?.branches ?? [];
  const [sel, setSel] = useState<string | null>(null);
  const selected = branches.find((b) => keyOf(b) === sel) ?? branches[0];

  return (
    <div className="mx-auto max-w-5xl space-y-5 pb-8">
      <header className="space-y-1">
        <h2 className="text-2xl font-semibold tracking-tight">Handoff</h2>
        <p className="text-sm text-muted-foreground">
          Per-branch resume packet — pick a branch to read or update its handoff.
        </p>
      </header>

      <div className="grid grid-cols-1 gap-4 md:grid-cols-[minmax(0,16rem)_minmax(0,1fr)]">
        <aside className="space-y-2">
          {notes.isLoading && (
            <div className="space-y-1.5" aria-hidden="true">
              {[0, 1, 2].map((i) => (
                <Skeleton key={i} className="h-14 w-full" />
              ))}
            </div>
          )}
          {notes.isError && (
            <Card className="flex items-center gap-2 border-urgent/30 bg-urgent-muted p-3 text-sm text-urgent">
              <AlertTriangle className="h-4 w-4" />
              Could not load branches.
            </Card>
          )}
          {!notes.isLoading && !notes.isError && branches.length === 0 && (
            <Card className="p-4 text-sm text-muted-foreground">No branch notes.</Card>
          )}
          {branches.length > 0 && (
            <BranchRail branches={branches} selected={selected} onSelect={setSel} />
          )}
        </aside>

        {selected ? (
          <HandoffPane entry={selected} />
        ) : (
          !notes.isLoading && (
            <Card className="flex items-center justify-center p-8 text-sm text-muted-foreground">
              Select a branch to view its handoff.
            </Card>
          )
        )}
      </div>

      <ToastStack />
    </div>
  );
}

/** The read + edit pane for one branch's handoff packet. */
function HandoffPane({ entry }: { entry: SummaryEntry }) {
  const qc = useQueryClient();
  const handoff = useQuery(handoffQuery(entry.repo, entry.branch));

  const [narrative, setNarrative] = useState("");
  const [priority, setPriority] = useState("");
  const [deferred, setDeferred] = useState("");
  const [questions, setQuestions] = useState("");
  const [dirty, setDirty] = useState(false);

  // Load the fetched packet into the editable drafts whenever a fresh branch's handoff loads.
  useEffect(() => {
    const h = handoff.data?.handoff;
    if (h) {
      setNarrative(h.narrative ?? "");
      setPriority((h.priority_queue ?? []).join("\n"));
      setDeferred((h.decisions?.deferred ?? []).join("\n"));
      setQuestions((h.open_questions ?? []).join("\n"));
      setDirty(false);
    }
  }, [handoff.data]);

  const save = useMutation({
    mutationFn: (): Promise<HandoffData> =>
      saveHandoff({
        repo: entry.repo,
        branch: entry.branch,
        narrative,
        deferred: lines(deferred),
        questions: lines(questions),
        priority_queue: lines(priority),
      }),
    onSuccess: () => {
      pushToast(`Saved handoff for ${entry.repo}/${entry.branch}`, "success");
      setDirty(false);
      void qc.invalidateQueries({ queryKey: queryKeys.handoff(entry.repo, entry.branch) });
    },
    onError: (e) => {
      pushToast(`Save failed: ${(e as Error).message}`, "error");
    },
  });

  const onEdit = (setter: (v: string) => void) => (v: string) => {
    setter(v);
    setDirty(true);
  };

  return (
    <Card className="space-y-4 p-4">
      <div className="flex flex-wrap items-center gap-2">
        <h3 className="font-mono text-sm font-semibold text-foreground">
          {entry.repo} / {entry.branch}
        </h3>
        {handoff.data?.handoff.written && (
          <Badge variant="muted">written {handoff.data.handoff.written}</Badge>
        )}
      </div>

      {handoff.isLoading && (
        <div className="flex items-center gap-2 text-sm text-muted-foreground">
          <Loader2 className="h-4 w-4 animate-spin" />
          Loading handoff…
        </div>
      )}
      {handoff.isError && (
        <Card className="flex items-center gap-2 border-urgent/30 bg-urgent-muted p-3 text-sm text-urgent">
          <AlertTriangle className="h-4 w-4" />
          Could not load handoff.
        </Card>
      )}

      {handoff.data && (
        <>
          <div className={cn("rounded-lg border border-border bg-muted/30 p-4", MARKDOWN_PROSE)}>
            <Markdown remarkPlugins={[remarkGfm]} components={MD_COMPONENTS}>
              {handoff.data.markdown}
            </Markdown>
          </div>

          <form
            className="space-y-4"
            onSubmit={(e) => {
              e.preventDefault();
              save.mutate();
            }}
          >
            <div className="space-y-1.5">
              <Label htmlFor="handoff-narrative">Narrative — why it looks this way</Label>
              <Textarea
                id="handoff-narrative"
                value={narrative}
                onChange={(e) => onEdit(setNarrative)(e.target.value)}
                rows={4}
                placeholder="A few sentences orienting the next session…"
              />
            </div>

            <div className="space-y-1.5">
              <Label htmlFor="handoff-priority">Do next — priority queue (one per line)</Label>
              <Textarea
                id="handoff-priority"
                className="font-mono text-[13px]"
                value={priority}
                spellCheck={false}
                onChange={(e) => onEdit(setPriority)(e.target.value)}
                rows={4}
                placeholder="Leave empty to default to the note's open todos"
              />
            </div>

            <div className="space-y-1.5">
              <Label htmlFor="handoff-deferred">Deferred decisions (one per line)</Label>
              <Textarea
                id="handoff-deferred"
                value={deferred}
                onChange={(e) => onEdit(setDeferred)(e.target.value)}
                rows={3}
              />
            </div>

            <div className="space-y-1.5">
              <Label htmlFor="handoff-questions">Open questions (one per line)</Label>
              <Textarea
                id="handoff-questions"
                value={questions}
                onChange={(e) => onEdit(setQuestions)(e.target.value)}
                rows={3}
              />
            </div>

            <div className="flex items-center gap-3">
              <Button type="submit" disabled={save.isPending}>
                {save.isPending ? "Saving…" : "Save handoff"}
              </Button>
              {dirty && <span className="text-xs text-muted-foreground">unsaved changes</span>}
            </div>
          </form>
        </>
      )}
    </Card>
  );
}
