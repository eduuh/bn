import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import {
  AlertTriangle,
  ArrowUpRight,
  Check,
  Circle,
  FileText,
  Link2,
  Paperclip,
  Target,
  Trash2,
} from "lucide-react";
import { goalsQuery, notesQuery, queryKeys } from "../api/client";
import type { DailyGoal, GoalNote, QuarterGoal, SummaryEntry } from "../api/types";
import { connectGoalNote, disconnectGoalNote } from "../api/writes";
import { Badge, type BadgeProps } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "../components/ui/dialog";
import { Input } from "../components/ui/input";
import { Label } from "../components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "../components/ui/select";
import { Skeleton } from "../components/ui/skeleton";
import { Textarea } from "../components/ui/textarea";
import { ToastStack } from "../components/ToastStack";
import { pushToast } from "../lib/toast";
import { cn } from "../lib/utils";

/** The deep-link to the Notes view for a connected branch note (the note-jump, per #561). */
function noteHref(repo: string, branch: string): string {
  return `/notes?repo=${encodeURIComponent(repo)}&branch=${encodeURIComponent(branch)}`;
}

/** A quarter goal's on-track tone. `off-track`/`at-risk` are amber→red; `on-track`/`done` green. */
const QUARTER_TONE: Record<string, BadgeProps["variant"]> = {
  "on-track": "ok",
  done: "ok",
  "at-risk": "warn",
  "off-track": "urgent",
  unscored: "muted",
};

/** A daily goal row: done state, aimed-vs-achieved minutes, partial progress, and its connected
 * notes (#564) with a per-goal "connect note" affordance. `branches` feeds the link picker. */
function DailyRow({ g, branches }: { g: DailyGoal; branches: SummaryEntry[] }) {
  return (
    <li
      className={cn(
        "flex flex-col gap-2 rounded-lg border border-border bg-card px-3 py-2.5 text-card-foreground shadow-card",
        g.done && "opacity-70",
      )}
    >
      <div className="flex items-center gap-3">
        <span
          className={cn(
            "inline-flex h-6 w-6 shrink-0 items-center justify-center rounded-full text-xs font-semibold tabular-nums",
            g.done ? "bg-ok-muted text-ok" : "bg-muted text-muted-foreground",
          )}
          aria-hidden="true"
        >
          {g.done ? <Check className="h-3.5 w-3.5" /> : g.pct != null ? `${g.pct}%` : <Circle className="h-3 w-3" />}
        </span>
        <span className={cn("min-w-0 flex-1 text-sm text-foreground", g.done && "line-through")}>
          {g.link ? (
            <a
              href={g.link}
              target="_blank"
              rel="noreferrer"
              className="inline-flex items-center gap-0.5 text-primary"
            >
              {g.text} <ArrowUpRight className="h-3 w-3" />
            </a>
          ) : (
            g.text
          )}
        </span>
        {(g.est != null || g.actual != null) && (
          <span className="shrink-0 font-mono text-xs text-muted-foreground" title="aimed vs achieved">
            {g.actual ?? "—"} / {g.est ?? "—"}
          </span>
        )}
        <ConnectNoteDialog goal={g} branches={branches} />
      </div>
      <GoalNotesList goal={g} />
    </li>
  );
}

/** The connected notes hanging off a goal (#564): a linked branch note jumps to the Notes view; an
 * attached text note shows its title/snippet. Each carries a remove (disconnect) control. */
function GoalNotesList({ goal }: { goal: DailyGoal }) {
  const qc = useQueryClient();
  const remove = useMutation({
    mutationFn: (noteId: string) => disconnectGoalNote(goal.id, noteId),
    onSuccess: () => {
      pushToast("Note disconnected", "success");
      void qc.invalidateQueries({ queryKey: queryKeys.goals });
    },
    onError: (e) => pushToast(`Could not disconnect: ${(e as Error).message}`, "error"),
  });

  const notes = goal.notes ?? [];
  if (notes.length === 0) return null;

  return (
    <ul className="ml-9 space-y-1">
      {notes.map((n) => (
        <li
          key={n.id}
          className="flex items-center gap-2 rounded-md border border-border/60 bg-muted/30 px-2 py-1 text-xs"
        >
          <NoteBody note={n} />
          <button
            type="button"
            onClick={() => remove.mutate(n.id)}
            disabled={remove.isPending}
            aria-label="Disconnect note"
            className="shrink-0 rounded p-0.5 text-muted-foreground hover:bg-accent hover:text-urgent"
          >
            <Trash2 className="h-3.5 w-3.5" />
          </button>
        </li>
      ))}
    </ul>
  );
}

/** One connected note's body: a jump link for a branch note, or the title/snippet for text. */
function NoteBody({ note }: { note: GoalNote }) {
  if (note.repo && note.branch) {
    return (
      <Link
        to={noteHref(note.repo, note.branch)}
        className="inline-flex min-w-0 flex-1 items-center gap-1 text-primary hover:underline"
        title={`${note.repo}/${note.branch}`}
      >
        <Link2 className="h-3.5 w-3.5 shrink-0" />
        <span className="truncate font-mono">
          {note.repo}/{note.branch}
        </span>
        <ArrowUpRight className="h-3 w-3 shrink-0" />
      </Link>
    );
  }
  return (
    <span className="inline-flex min-w-0 flex-1 items-center gap-1 text-foreground">
      <FileText className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
      <span className="min-w-0">
        {note.title && <span className="font-medium">{note.title}: </span>}
        <span className="text-muted-foreground">{note.text}</span>
      </span>
    </span>
  );
}

/** The per-goal "connect note" affordance (#564): a dialog to link an existing branch note or
 * attach free-text / uploaded content. On success it invalidates the goals query so the note
 * renders immediately. */
function ConnectNoteDialog({ goal, branches }: { goal: DailyGoal; branches: SummaryEntry[] }) {
  const qc = useQueryClient();
  const [open, setOpen] = useState(false);
  const [mode, setMode] = useState<"link" | "text">("link");
  const [selected, setSelected] = useState<string>("");
  const [title, setTitle] = useState("");
  const [text, setText] = useState("");

  const reset = () => {
    setMode(branches.length > 0 ? "link" : "text");
    setSelected("");
    setTitle("");
    setText("");
  };

  const connect = useMutation({
    mutationFn: () => {
      if (mode === "link") {
        const [repo, branch] = selected.split("/", 2);
        return connectGoalNote(goal.id, { repo, branch });
      }
      return connectGoalNote(goal.id, { text, title: title || undefined });
    },
    onSuccess: () => {
      pushToast("Note connected", "success");
      void qc.invalidateQueries({ queryKey: queryKeys.goals });
      setOpen(false);
      reset();
    },
    onError: (e) => pushToast(`Could not connect: ${(e as Error).message}`, "error"),
  });

  const canSubmit =
    mode === "link" ? selected.includes("/") : text.trim().length > 0;

  // On a file pick, read its text into the attachment (small docs only; the server caps at 100 KB).
  const onFile = async (file: File | undefined) => {
    if (!file) return;
    setMode("text");
    setTitle((t) => t || file.name);
    setText(await file.text());
  };

  return (
    <Dialog
      open={open}
      onOpenChange={(o) => {
        setOpen(o);
        if (o) reset();
      }}
    >
      <DialogTrigger asChild>
        <Button variant="ghost" size="sm" className="h-7 shrink-0 gap-1 px-2 text-xs">
          <Paperclip className="h-3.5 w-3.5" />
          Connect note
        </Button>
      </DialogTrigger>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>Connect a note</DialogTitle>
          <DialogDescription className="truncate">{goal.text}</DialogDescription>
        </DialogHeader>

        <div className="flex gap-2">
          <Button
            type="button"
            variant={mode === "link" ? "default" : "outline"}
            size="sm"
            onClick={() => setMode("link")}
            disabled={branches.length === 0}
          >
            <Link2 className="mr-1 h-3.5 w-3.5" /> Link a branch note
          </Button>
          <Button
            type="button"
            variant={mode === "text" ? "default" : "outline"}
            size="sm"
            onClick={() => setMode("text")}
          >
            <FileText className="mr-1 h-3.5 w-3.5" /> Attach text
          </Button>
        </div>

        <form
          className="space-y-4"
          onSubmit={(e) => {
            e.preventDefault();
            if (canSubmit) connect.mutate();
          }}
        >
          {mode === "link" ? (
            <div className="space-y-1.5">
              <Label htmlFor="goal-note-branch">Branch note</Label>
              {branches.length === 0 ? (
                <p className="text-sm text-muted-foreground">No branch notes to link.</p>
              ) : (
                <Select value={selected} onValueChange={setSelected}>
                  <SelectTrigger id="goal-note-branch">
                    <SelectValue placeholder="Pick a branch note…" />
                  </SelectTrigger>
                  <SelectContent>
                    {branches.map((b) => {
                      const k = `${b.repo}/${b.branch}`;
                      return (
                        <SelectItem key={k} value={k}>
                          <span className="font-mono">{k}</span>
                        </SelectItem>
                      );
                    })}
                  </SelectContent>
                </Select>
              )}
            </div>
          ) : (
            <>
              <div className="space-y-1.5">
                <Label htmlFor="goal-note-title">Title (optional)</Label>
                <Input
                  id="goal-note-title"
                  value={title}
                  onChange={(e) => setTitle(e.target.value)}
                  placeholder="e.g. spec.md"
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="goal-note-text">Note content</Label>
                <Textarea
                  id="goal-note-text"
                  value={text}
                  onChange={(e) => setText(e.target.value)}
                  rows={5}
                  placeholder="Paste supporting context, or upload a file below…"
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="goal-note-file">Or upload a text file</Label>
                <Input
                  id="goal-note-file"
                  type="file"
                  accept=".txt,.md,.markdown,text/*"
                  onChange={(e) => void onFile(e.target.files?.[0])}
                />
              </div>
            </>
          )}

          <DialogFooter>
            <Button type="submit" disabled={!canSubmit || connect.isPending}>
              {connect.isPending ? "Connecting…" : "Connect"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** A quarter goal row: derived on-track status + its target metric. */
function QuarterRow({ g }: { g: QuarterGoal }) {
  return (
    <li
      className={cn(
        "flex items-center gap-3 rounded-lg border border-border bg-card px-3 py-2.5 text-card-foreground shadow-card",
        g.done && "opacity-70",
      )}
    >
      <Badge variant={QUARTER_TONE[g.status] ?? "muted"} className="shrink-0">
        {g.status}
      </Badge>
      <span className={cn("min-w-0 flex-1 text-sm text-foreground", g.done && "line-through")}>
        {g.text}
      </span>
      {g.metric && (
        <span className="shrink-0 font-mono text-xs text-muted-foreground">{g.metric}</span>
      )}
    </li>
  );
}

/** An inline empty note within a section (no goals set). */
function SectionEmpty({ children }: { children: React.ReactNode }) {
  return (
    <Card className="px-4 py-6 text-center text-sm text-muted-foreground">{children}</Card>
  );
}

/**
 * `/goals` — the daily + quarter goal boards (#483). Daily goals show aimed-vs-achieved (est /
 * actual minutes) and partial progress; quarter goals show their derived on-track status against
 * the elapsed fraction of the quarter. Daily goals can connect/upload supporting notes (#564) — a
 * write — but adding/closing goals stays a `bn daily`/`bn quarter` action.
 */
export function Goals() {
  const goals = useQuery(goalsQuery());
  const notes = useQuery(notesQuery());
  const branches = notes.data?.branches ?? [];
  const daily = goals.data?.daily;
  const quarter = goals.data?.quarter;

  return (
    <div className="mx-auto max-w-3xl space-y-6 pb-8">
      <header className="space-y-1">
        <h2 className="text-2xl font-semibold tracking-tight">Goals</h2>
        <p className="text-sm text-muted-foreground">Today &amp; this quarter — aimed vs achieved.</p>
      </header>

      {goals.isLoading && (
        <div className="space-y-3" aria-hidden="true">
          <Skeleton className="h-5 w-24" />
          {[0, 1, 2].map((i) => (
            <Skeleton key={i} className="h-12 w-full" />
          ))}
        </div>
      )}

      {goals.isError && (
        <Card className="flex items-center gap-2 border-urgent/30 bg-urgent-muted p-4 text-sm text-urgent">
          <AlertTriangle className="h-4 w-4" />
          Could not load goals.
        </Card>
      )}

      {daily && (
        <section className="space-y-3">
          <div className="flex items-center gap-2">
            <h3 className="text-sm font-semibold uppercase tracking-wide text-muted-foreground">Today</h3>
            <span className="text-xs text-muted-foreground">
              {daily.date} · {daily.done}/{daily.goals.length} done
            </span>
          </div>
          {daily.goals.length === 0 ? (
            <SectionEmpty>
              No daily goals set. Add them with <code className="font-mono">bn daily add</code>.
            </SectionEmpty>
          ) : (
            <ul className="space-y-2">
              {daily.goals.map((g) => (
                <DailyRow key={g.id} g={g} branches={branches} />
              ))}
            </ul>
          )}
          {daily.upcoming.length > 0 && (
            <p className="text-xs text-muted-foreground">
              Upcoming: {daily.upcoming.map((u) => u.text).join(" · ")}
            </p>
          )}
        </section>
      )}

      {quarter && (
        <section className="space-y-3">
          <div className="flex items-center gap-2">
            <Target className="h-4 w-4 text-muted-foreground" />
            <h3 className="text-sm font-semibold uppercase tracking-wide text-muted-foreground">
              {quarter.quarter ?? "Quarter"}
            </h3>
            <span className="text-xs text-muted-foreground">
              {quarter.on_track}/{quarter.total} on track
              {quarter.elapsed_pct != null && ` · ${quarter.elapsed_pct}% elapsed`}
            </span>
          </div>
          {quarter.elapsed_pct != null && (
            <div
              className="h-1.5 w-full overflow-hidden rounded-full bg-muted"
              role="progressbar"
              aria-valuenow={quarter.elapsed_pct}
              aria-valuemin={0}
              aria-valuemax={100}
              aria-label="quarter elapsed"
            >
              <span
                className="block h-full rounded-full bg-info"
                style={{ width: `${quarter.elapsed_pct}%` }}
              />
            </div>
          )}
          {quarter.goals.length === 0 ? (
            <SectionEmpty>
              No quarter goals set. Add them with <code className="font-mono">bn quarter add</code>.
            </SectionEmpty>
          ) : (
            <ul className="space-y-2">
              {quarter.goals.map((g) => (
                <QuarterRow key={g.key} g={g} />
              ))}
            </ul>
          )}
        </section>
      )}

      <ToastStack />
    </div>
  );
}
