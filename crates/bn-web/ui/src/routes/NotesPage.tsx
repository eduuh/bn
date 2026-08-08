import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { useSearchParams } from "react-router-dom";
import { AlertTriangle } from "lucide-react";
import { notesQuery, noteDetailQuery } from "../api/client";
import type { SummaryEntry } from "../api/types";
import { LearningsFeed } from "../components/LearningsFeed";
import { BnFileBrowser, PlanMarkdownViewer, PlanSummaryCard } from "../components/PlanArtifacts";
import { ToastStack } from "../components/ToastStack";
import { Badge } from "../components/ui/badge";
import { Card } from "../components/ui/card";
import { Skeleton } from "../components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "../components/ui/tabs";
import { cn } from "../lib/utils";
import { PageHead } from "../components/PageHead";

const keyOf = (b: SummaryEntry): string => `${b.repo}/${b.branch}`;

/**
 * `/notes` — Notes & Learnings (design §4(e)). Two tabs: a branch-note browser (rail of rollups +
 * a rendered, read-only note) and the learnings review feed. Read-only v1 (#322 for promote/
 * dismiss wiring); the rail hydrates from `/api/notes`, a selected note from `/api/notes/:repo/:b`.
 */
export function NotesPage() {
  return (
    <div className="space-y-5 pb-8">
      <PageHead title="Notes & Learnings" sub="branch notes · captured learnings" />

      <Tabs defaultValue="notes">
        <TabsList>
          <TabsTrigger value="notes">Notes</TabsTrigger>
          <TabsTrigger value="learnings">Learnings</TabsTrigger>
        </TabsList>
        <TabsContent value="notes">
          <NotesBrowser />
        </TabsContent>
        <TabsContent value="learnings">
          <LearningsFeed />
        </TabsContent>
      </Tabs>

      <ToastStack />
    </div>
  );
}

/** The notes tab: a selectable branch rail + the rendered note for the selection. A `?repo=&branch=`
 * deep-link (the note-jump from Goals, #564/#561) preselects that note until the user picks another. */
function NotesBrowser() {
  const notes = useQuery(notesQuery());
  const branches = notes.data?.branches ?? [];
  const [params] = useSearchParams();
  const wanted =
    params.get("repo") && params.get("branch")
      ? `${params.get("repo")}/${params.get("branch")}`
      : null;
  const [sel, setSel] = useState<string | null>(null);
  const selected =
    branches.find((b) => keyOf(b) === sel) ??
    branches.find((b) => keyOf(b) === wanted) ??
    branches[0];

  return (
    <div className="grid grid-cols-1 gap-4 md:grid-cols-[minmax(0,16rem)_minmax(0,1fr)]">
      <aside className="space-y-2">
        {notes.isLoading && (
          <div className="space-y-1.5" aria-hidden="true">
            {[0, 1, 2].map((i) => (
              <Skeleton key={i} className="h-16 w-full" />
            ))}
          </div>
        )}
        {notes.isError && (
          <Card className="flex items-center gap-2 border-urgent/30 bg-urgent-muted p-3 text-sm text-urgent">
            <AlertTriangle className="h-4 w-4" />
            Could not load notes.
          </Card>
        )}
        {!notes.isLoading && !notes.isError && branches.length === 0 && (
          <Card className="p-4 text-sm text-muted-foreground">No branch notes.</Card>
        )}
        <ul className="space-y-1.5">
          {branches.map((b) => {
            const k = keyOf(b);
            const active = selected && keyOf(selected) === k;
            return (
              <li key={k}>
                <button
                  type="button"
                  onClick={() => setSel(k)}
                  className={cn(
                    "w-full rounded-lg border px-3 py-2 text-left transition-colors",
                    active
                      ? "border-primary/40 bg-primary/10"
                      : "border-border bg-card hover:border-border/80 hover:bg-accent/40",
                  )}
                >
                  {/* Branch first. This list is one repo's branches far more often than not, so
                      leading with the repo made every row open with the same word and demoted the
                      only field that tells them apart to a grey subtitle. `break-all` because the
                      branch is the identifier — truncating three `dispatch-pick-up-this-open-tod…`
                      rows to the same string makes them literally indistinguishable. */}
                  <div className="flex items-start justify-between gap-2">
                    <span className="min-w-0 break-all font-mono text-sm font-medium text-foreground">
                      {b.branch}
                    </span>
                    {/* Only a status worth reading. Every note here is "active", and a badge that
                        is on every row carries no information — it just adds a column of noise. */}
                    {b.status && b.status !== "active" && (
                      <Badge variant="muted" className="shrink-0">
                        {b.status}
                      </Badge>
                    )}
                  </div>
                  <div className="mt-0.5 truncate font-mono text-xs text-muted-foreground">
                    {b.repo}
                  </div>
                  {(b.todos.length > 0 || b.blockers.length > 0) && (
                    <div className="mt-1.5 flex flex-wrap gap-1.5">
                      {b.todos.length > 0 && <Badge variant="muted">{b.todos.length} todo</Badge>}
                      {b.blockers.length > 0 && (
                        <Badge variant="urgent">{b.blockers.length} blocked</Badge>
                      )}
                    </div>
                  )}
                </button>
              </li>
            );
          })}
        </ul>
      </aside>

      {selected ? (
        <NoteView entry={selected} />
      ) : (
        !notes.isLoading && (
          <Card className="flex items-center justify-center p-8 text-sm text-muted-foreground">
            Select a branch note to read it.
          </Card>
        )
      )}
    </div>
  );
}

/** The rendered note for one branch: goal line, blockers/open todos (read-only checkboxes), and
 * the raw note body. `entry` supplies the rollup counts; the body + goal come from the detail. */
function NoteView({ entry }: { entry: SummaryEntry }) {
  const note = useQuery(noteDetailQuery(entry.repo, entry.branch));

  return (
    <div className="space-y-4">
      <Card className="space-y-4 p-4">
        <div className="flex flex-wrap items-center gap-2">
          <h3 className="font-mono text-sm font-semibold text-foreground">
            {entry.repo} / {entry.branch}
          </h3>
          {entry.status && <Badge variant="muted">{entry.status}</Badge>}
        </div>

        {note.data?.goal && (
          <p className="flex flex-wrap items-baseline gap-2 text-sm text-foreground">
            <span className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
              Goal
            </span>
            {note.data.goal}
          </p>
        )}
      </Card>

      <PlanSummaryCard repo={entry.repo} branch={entry.branch} />

      <Tabs defaultValue="plan">
        <TabsList>
          <TabsTrigger value="plan">Plan</TabsTrigger>
          <TabsTrigger value="files">Files</TabsTrigger>
          <TabsTrigger value="note">Note</TabsTrigger>
        </TabsList>
        <TabsContent value="plan">
          <PlanMarkdownViewer repo={entry.repo} branch={entry.branch} />
        </TabsContent>
        <TabsContent value="files">
          <BnFileBrowser repo={entry.repo} branch={entry.branch} />
        </TabsContent>
        <TabsContent value="note">
          <Card className="space-y-4 p-4">
            {(entry.blockers.length > 0 || entry.todos.length > 0) && (
              <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
                {entry.blockers.length > 0 && (
                  <div className="space-y-2">
                    <h4 className="text-sm font-semibold text-foreground">Blockers</h4>
                    <ul className="space-y-1 text-sm text-urgent">
                      {entry.blockers.map((x) => (
                        <li key={x.id} className="flex items-start gap-2">
                          <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
                          <span className="min-w-0 flex-1">{x.text}</span>
                        </li>
                      ))}
                    </ul>
                  </div>
                )}
                {entry.todos.length > 0 && (
                  <div className="space-y-2">
                    <h4 className="text-sm font-semibold text-foreground">Open todos</h4>
                    <ul className="space-y-1 text-sm">
                      {entry.todos.map((x) => (
                        <li key={x.id} className="flex items-start gap-2">
                          <input
                            type="checkbox"
                            disabled
                            readOnly
                            className="mt-1 h-3.5 w-3.5 shrink-0 accent-primary"
                          />
                          <span className="min-w-0 flex-1 text-foreground/90">{x.text}</span>
                        </li>
                      ))}
                    </ul>
                  </div>
                )}
              </div>
            )}

            {note.isLoading && (
              <div className="space-y-2" aria-hidden="true">
                <Skeleton className="h-4 w-2/3" />
                <Skeleton className="h-4 w-1/2" />
                <Skeleton className="h-24 w-full" />
              </div>
            )}
            {note.isError && (
              <Card className="flex items-center gap-2 border-urgent/30 bg-urgent-muted p-3 text-sm text-urgent">
                <AlertTriangle className="h-4 w-4" />
                Could not load note.
              </Card>
            )}
            {note.data?.body && (
              <pre className="overflow-auto rounded-md border border-border bg-muted/40 p-3 font-mono text-xs leading-relaxed text-foreground/90">
                {note.data.body}
              </pre>
            )}
          </Card>
        </TabsContent>
      </Tabs>
    </div>
  );
}
