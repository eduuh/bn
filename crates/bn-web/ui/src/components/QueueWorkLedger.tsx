import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { ChevronLeft, ChevronRight, Search } from "lucide-react";
import { Link } from "react-router-dom";

import { taskLedgerQuery, type TaskLedgerState } from "../api/client";
import type { TaskRow } from "../api/types";
import { Badge, type BadgeProps } from "./ui/badge";
import { Button } from "./ui/button";
import { Input } from "./ui/input";
import { LoadError, RowsSkeleton, SectionCard } from "./SectionCard";

const PAGE_SIZE = 40;
const STATES: TaskLedgerState[] = ["all", "open", "wip", "snoozed", "done", "cancelled"];

const STATUS_TONE: Record<string, BadgeProps["variant"]> = {
  open: "muted",
  wip: "info",
  snoozed: "warn",
  done: "ok",
  cancelled: "secondary",
};

function taskStamp(task: TaskRow): string {
  if (task.completion) return `completed ${task.completion}`;
  if (task.day) return `planned ${task.day}`;
  if (task.due) return `due ${task.due}`;
  if (task.mod > 0) return new Date(task.mod * 1000).toLocaleDateString();
  return task.created ?? "undated";
}

function LedgerRow({ task }: { task: TaskRow }) {
  const body = (
    <>
      <Badge variant={STATUS_TONE[task.status] ?? "muted"} className="shrink-0">
        {task.status}
      </Badge>
      <span className="min-w-0 flex-1">
        <span className="block truncate text-sm font-medium text-foreground" title={task.summary}>
          {task.summary}
        </span>
        <span className="block truncate font-mono text-[11px] text-muted-foreground">
          {[task.repo, task.branch, task.stage].filter(Boolean).join(" / ") || "unscoped"}
        </span>
      </span>
      <span className="shrink-0 text-right">
        <span className="block font-mono text-[11px] text-muted-foreground">{task.id}</span>
        <span className="block text-[11px] text-muted-foreground">{taskStamp(task)}</span>
      </span>
    </>
  );

  return task.id ? (
    <Link
      to={`/tasks/${encodeURIComponent(task.id)}`}
      className="flex items-center gap-3 px-1 py-2 transition-colors hover:bg-accent/30"
    >
      {body}
    </Link>
  ) : (
    <div className="flex items-center gap-3 px-1 py-2">{body}</div>
  );
}

/** Searchable, paged access to the task file's complete lifecycle history. */
export function QueueWorkLedger() {
  const [state, setState] = useState<TaskLedgerState>("all");
  const [rawQuery, setRawQuery] = useState("");
  const [rawRepo, setRawRepo] = useState("");
  const [query, setQuery] = useState("");
  const [repo, setRepo] = useState("");
  const [offset, setOffset] = useState(0);

  useEffect(() => {
    const timer = setTimeout(() => {
      setQuery(rawQuery.trim());
      setRepo(rawRepo.trim());
      setOffset(0);
    }, 180);
    return () => clearTimeout(timer);
  }, [rawQuery, rawRepo]);

  const ledger = useQuery(taskLedgerQuery({ state, query, repo, offset, limit: PAGE_SIZE }));
  const view = ledger.data;
  const allCount = Object.values(view?.counts ?? {}).reduce((sum, count) => sum + count, 0);
  const pageStart = view && view.count > 0 ? view.offset + 1 : 0;
  const pageEnd = view ? view.offset + view.count : 0;

  return (
    <SectionCard
      eyebrow="all time"
      title="Work inventory"
      count={view ? `${view.total} matching` : undefined}
    >
      <div className="mb-3 flex flex-wrap items-center gap-2">
        <div className="flex flex-wrap gap-1" role="group" aria-label="Task lifecycle">
          {STATES.map((value) => (
            <Button
              key={value}
              size="sm"
              variant={state === value ? "default" : "secondary"}
              aria-pressed={state === value}
              onClick={() => {
                setState(value);
                setOffset(0);
              }}
            >
              {value}
              <span className="ml-1 opacity-70">
                {value === "all" ? allCount : (view?.counts[value] ?? 0)}
              </span>
            </Button>
          ))}
        </div>
        <label className="relative min-w-56 flex-1">
          <Search className="pointer-events-none absolute left-2.5 top-2 size-4 text-muted-foreground" />
          <Input
            value={rawQuery}
            onChange={(event) => setRawQuery(event.target.value)}
            placeholder="search every task field"
            aria-label="Search work inventory"
            className="h-8 pl-8"
          />
        </label>
        <Input
          value={rawRepo}
          onChange={(event) => setRawRepo(event.target.value)}
          placeholder="repo"
          aria-label="Filter inventory by repo"
          className="h-8 w-40"
        />
      </div>

      {ledger.isError ? (
        <LoadError>Could not load the work inventory.</LoadError>
      ) : ledger.isLoading && !view ? (
        <RowsSkeleton rows={5} />
      ) : (view?.tasks.length ?? 0) === 0 ? (
        <p className="py-3 text-sm text-muted-foreground">
          No work matches these inventory filters.
        </p>
      ) : (
        <div className="divide-y divide-border">
          {view?.tasks.map((task) => (
            <LedgerRow key={task.uid ?? task.id} task={task} />
          ))}
        </div>
      )}

      {view && view.total > 0 ? (
        <div className="mt-3 flex items-center justify-between border-t border-border pt-3">
          <span className="font-mono text-[11px] tabular-nums text-muted-foreground">
            {pageStart}–{pageEnd} of {view.total}
          </span>
          <div className="flex gap-1">
            <Button
              size="sm"
              variant="secondary"
              disabled={view.offset === 0}
              onClick={() => setOffset(Math.max(0, view.offset - view.limit))}
            >
              <ChevronLeft />
              Previous
            </Button>
            <Button
              size="sm"
              variant="secondary"
              disabled={pageEnd >= view.total}
              onClick={() => setOffset(view.offset + view.limit)}
            >
              Next
              <ChevronRight />
            </Button>
          </div>
        </div>
      ) : null}
    </SectionCard>
  );
}
