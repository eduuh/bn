import { Fragment, useMemo, useState, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { AlertTriangle, CircleDot, Download, FileJson, Play, Search } from "lucide-react";
import { Link } from "react-router-dom";

import { queueEventsQuery } from "../api/client";
import type { QueueEventRow, QueueRunRow, QueueRunTransition } from "../api/queue-types";
import { Empty, LoadError, RowsSkeleton, SectionCard } from "../components/SectionCard";
import { Badge, type BadgeProps } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Input } from "../components/ui/input";
import { cn } from "../lib/utils";

const WINDOWS = ["30m", "4h", "1d", "7d"] as const;
const FAILURE_OUTCOMES = new Set(["failed", "expired", "reclaimed"]);
const HELD_OUTCOMES = new Set(["held", "skipped"]);

function clock(at: number): string {
  return new Date(at * 1000).toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

function dayLabel(at: number): string {
  return new Date(at * 1000).toLocaleDateString(undefined, {
    weekday: "short",
    month: "short",
    day: "numeric",
  });
}

function fmtSecs(secs: number, live = false): string {
  const suffix = live ? " live" : "";
  if (secs < 1) return `<1s${suffix}`;
  if (secs < 90) return `${secs}s${suffix}`;
  const mins = Math.floor(secs / 60);
  const rem = secs % 60;
  return `${mins}m${rem > 0 ? ` ${rem}s` : ""}${suffix}`;
}

function outcomeTone(outcome: string): BadgeProps["variant"] {
  if (outcome === "completed") return "ok";
  if (outcome === "in-flight") return "info";
  if (FAILURE_OUTCOMES.has(outcome)) return "urgent";
  if (HELD_OUTCOMES.has(outcome)) return "warn";
  return "muted";
}

function SummaryMetric({ label, value, tone }: { label: string; value: ReactNode; tone?: string }) {
  return (
    <div className="bg-card px-3 py-3">
      <strong className={cn("block font-mono text-xl tabular-nums", tone)}>{value}</strong>
      <span className="text-[11px] text-muted-foreground">{label}</span>
    </div>
  );
}

function eventOutcome(row: QueueEventRow): string {
  if (row.kind === "dispatch") return "in-flight";
  if (row.kind === "finish") {
    const recorded = fieldString(row.fields, "outcome");
    return recorded || (Number(row.fields.exit_code ?? 0) === 0 ? "completed" : "failed");
  }
  if (row.kind === "tick") return fieldString(row.fields, "outcome") || "decision";
  if (row.kind === "expired") return "expired";
  if (row.kind === "reclaimed") return "reclaimed";
  if (row.kind === "skipped") return "skipped";
  return row.kind;
}

function csvCell(value: unknown): string {
  const text = typeof value === "string" ? value : JSON.stringify(value);
  return `"${text.replace(/"/g, '""')}"`;
}

function downloadCsv(name: string, rows: unknown[][]) {
  const csv = rows.map((row) => row.map(csvCell).join(",")).join("\n");
  const url = URL.createObjectURL(new Blob([csv], { type: "text/csv;charset=utf-8" }));
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = name;
  anchor.click();
  URL.revokeObjectURL(url);
}

function fieldString(fields: Record<string, unknown>, key: string): string {
  const value = fields[key];
  return typeof value === "string" ? value : "";
}

function RunTable({
  rows,
  selectedKey,
  onSelect,
}: {
  rows: QueueRunRow[];
  selectedKey?: string;
  onSelect: (key: string) => void;
}) {
  if (rows.length === 0) {
    return <Empty>No runs match these filters.</Empty>;
  }

  let previousDay = "";
  return (
    <div className="overflow-x-auto">
      <table className="w-full min-w-[760px] border-collapse text-left text-sm">
        <thead>
          <tr className="border-b border-border text-[11px] uppercase tracking-wide text-muted-foreground">
            <th className="px-2 py-2 font-medium">Started</th>
            <th className="px-2 py-2 font-medium">Work</th>
            <th className="px-2 py-2 font-medium">Route</th>
            <th className="px-2 py-2 font-medium">Outcome</th>
            <th className="px-2 py-2 text-right font-medium">Duration</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((run) => {
            const day = dayLabel(run.started_at);
            const showDay = day !== previousDay;
            previousDay = day;
            return (
              <Fragment key={run.key}>
                {showDay ? (
                  <tr className="border-b border-border bg-muted/30">
                    <td
                      colSpan={5}
                      className="px-2 py-1.5 text-[10px] font-medium uppercase tracking-[0.12em] text-muted-foreground"
                    >
                      {day}
                    </td>
                  </tr>
                ) : null}
                <tr
                  role="button"
                  tabIndex={0}
                  aria-selected={selectedKey === run.key}
                  className={cn(
                    "cursor-pointer border-b border-border/70 transition-colors hover:bg-accent/30",
                    selectedKey === run.key && "bg-accent/40",
                  )}
                  onClick={() => onSelect(run.key)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter" || event.key === " ") {
                      event.preventDefault();
                      onSelect(run.key);
                    }
                  }}
                >
                  <td className="whitespace-nowrap px-2 py-2.5 font-mono text-xs text-muted-foreground">
                    {clock(run.started_at)}
                  </td>
                  <td className="max-w-md px-2 py-2.5">
                    <span className="block truncate font-medium" title={run.work}>
                      {run.id || run.work}
                    </span>
                    <span className="block truncate text-[11px] text-muted-foreground">
                      {run.work}
                    </span>
                  </td>
                  <td className="whitespace-nowrap px-2 py-2.5 text-xs text-muted-foreground">
                    {(run.repo || "machine") + " → " + (run.node || "unassigned")}
                  </td>
                  <td className="px-2 py-2.5">
                    <Badge variant={outcomeTone(run.outcome)}>{run.outcome}</Badge>
                  </td>
                  <td className="whitespace-nowrap px-2 py-2.5 text-right font-mono text-xs">
                    {fmtSecs(run.duration_secs, run.ended_at == null)}
                  </td>
                </tr>
              </Fragment>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

function TransitionRow({ transition }: { transition: QueueRunTransition }) {
  return (
    <li className="grid gap-2 border-l-2 border-border py-2 pl-3 sm:grid-cols-[7rem_7rem_minmax(0,1fr)]">
      <time className="font-mono text-xs tabular-nums text-muted-foreground">
        {clock(transition.at)}
      </time>
      <Badge
        variant={
          transition.tone === "bad" ? "urgent" : transition.tone === "start" ? "ok" : "muted"
        }
        className="w-fit"
      >
        {transition.kind}
      </Badge>
      <div className="min-w-0">
        <p className="text-sm">{transition.summary}</p>
        {Object.keys(transition.fields).length > 0 ? (
          <pre className="mt-1 overflow-x-auto whitespace-pre-wrap rounded-md bg-muted/50 p-2 font-mono text-[10px] leading-relaxed text-muted-foreground">
            {JSON.stringify(transition.fields, null, 2)}
          </pre>
        ) : null}
      </div>
    </li>
  );
}

function recordedEvidence(run: QueueRunRow): string {
  const recorded: string[] = [];
  for (const transition of run.transitions) {
    for (const key of ["output", "stdout", "stderr", "error"]) {
      const value = transition.fields[key];
      if (typeof value === "string" && value.trim()) {
        recorded.push(`${transition.kind}.${key}:\n${value.trim()}`);
      }
    }
  }
  const heading =
    recorded.length > 0 ? recorded.join("\n\n") : "No stdout or stderr was recorded for this run.";
  return `${heading}\n\nStructured event payload:\n${JSON.stringify(
    run.transitions.map((transition) => ({
      at: transition.at,
      kind: transition.kind,
      fields: transition.fields,
    })),
    null,
    2,
  )}`;
}

function RunInspector({
  run,
  onShowRaw,
}: {
  run?: QueueRunRow;
  onShowRaw: (run: QueueRunRow) => void;
}) {
  if (!run) {
    return (
      <SectionCard eyebrow="evidence" title="Run detail">
        <Empty>Select a run to inspect its complete recorded evidence.</Empty>
      </SectionCard>
    );
  }

  const facts = [
    ["Outcome", run.outcome],
    ["Node", run.node || "not assigned"],
    ["Lane", run.lane || "not recorded"],
    ["Duration", fmtSecs(run.duration_secs, run.ended_at == null)],
    ["Executor", [run.exec, run.harness].filter(Boolean).join(" / ") || "not recorded"],
    ["Entry now", run.entry_status || "not recorded"],
  ];

  return (
    <SectionCard eyebrow="evidence" title="Run detail" count={run.key}>
      <div className="border-b border-border pb-3">
        <p className="font-mono text-xs text-muted-foreground">
          {run.repo || "machine"} / {run.id || run.work_kind}
        </p>
        <h2 className="mt-1 text-lg font-semibold">{run.work}</h2>
        <p className="mt-1 text-sm text-muted-foreground">{run.summary}</p>
        {run.rationale ? (
          <p className="mt-2 text-sm">
            <span className="font-medium">Why it ran:</span> {run.rationale}
          </p>
        ) : null}
      </div>

      <div className="grid overflow-hidden border-b border-border bg-border sm:grid-cols-3 lg:grid-cols-6">
        {facts.map(([label, value]) => (
          <div key={label} className="bg-card px-3 py-2.5">
            <span className="block text-[10px] uppercase tracking-wide text-muted-foreground">
              {label}
            </span>
            <strong className="mt-1 block truncate text-sm" title={value}>
              {value}
            </strong>
          </div>
        ))}
      </div>

      <div className="py-3">
        <h3 className="mb-2 text-xs font-medium uppercase tracking-wide text-muted-foreground">
          Transition chain
        </h3>
        <ol className="space-y-1">
          {run.transitions.map((transition, index) => (
            <TransitionRow
              key={`${transition.at}:${transition.kind}:${index}`}
              transition={transition}
            />
          ))}
        </ol>
      </div>

      <div className="border-t border-border pt-3">
        <h3 className="mb-2 text-xs font-medium uppercase tracking-wide text-muted-foreground">
          Outcome and recorded output
        </h3>
        <pre className="max-h-[32rem] overflow-auto whitespace-pre-wrap rounded-md border border-border bg-muted/40 p-3 font-mono text-xs leading-relaxed">
          {recordedEvidence(run)}
        </pre>
        <div className="mt-3 flex flex-wrap gap-2">
          {run.task ? (
            <Button asChild size="sm" variant="secondary">
              <Link to={`/tasks/${encodeURIComponent(run.task)}`}>Open task {run.task}</Link>
            </Button>
          ) : null}
          <Button size="sm" variant="secondary" onClick={() => onShowRaw(run)}>
            <FileJson />
            Raw events
          </Button>
        </div>
      </div>
    </SectionCard>
  );
}

function RawEvents({ rows }: { rows: QueueEventRow[] }) {
  if (rows.length === 0) {
    return <Empty>No raw events match these filters.</Empty>;
  }
  return (
    <div className="divide-y divide-border">
      {rows.map((row, index) => (
        <details key={`${row.at}:${row.kind}:${row.id ?? ""}:${index}`} className="group py-2">
          <summary className="flex cursor-pointer list-none flex-wrap items-center gap-2 text-sm">
            <time className="font-mono text-xs text-muted-foreground">{clock(row.at)}</time>
            <Badge variant={row.tone === "bad" ? "urgent" : row.tone === "start" ? "ok" : "muted"}>
              {row.kind}
            </Badge>
            {row.repo || row.id ? (
              <span className="font-mono text-xs text-muted-foreground">
                {[row.repo, row.id].filter(Boolean).join("/")}
              </span>
            ) : null}
            <span className="min-w-64 flex-1">{row.summary}</span>
          </summary>
          <pre className="mt-2 overflow-x-auto whitespace-pre-wrap rounded-md bg-muted/50 p-3 font-mono text-xs">
            {JSON.stringify(row.fields, null, 2)}
          </pre>
        </details>
      ))}
    </div>
  );
}

export function QueueEvents() {
  const [since, setSince] = useState<string>("4h");
  const [presentation, setPresentation] = useState<"runs" | "raw">("runs");
  const [search, setSearch] = useState("");
  const [outcome, setOutcome] = useState("");
  const [workKind, setWorkKind] = useState("");
  const [node, setNode] = useState("");
  const [repo, setRepo] = useState("");
  const [selectedKey, setSelectedKey] = useState<string>();
  const events = useQuery(queueEventsQuery({ since, kind: "", repo: "" }));

  const allRuns = useMemo(() => [...(events.data?.runs ?? [])].reverse(), [events.data]);
  const allRaw = useMemo(() => [...(events.data?.events ?? [])].reverse(), [events.data]);
  const query = search.trim().toLowerCase();

  const filteredRuns = useMemo(
    () =>
      allRuns.filter((run) => {
        const haystack = [
          run.key,
          run.repo,
          run.id,
          run.work,
          run.work_kind,
          run.node,
          run.summary,
          run.rationale,
          run.task,
        ]
          .join(" ")
          .toLowerCase();
        return (
          (!query || haystack.includes(query)) &&
          (!outcome || run.outcome === outcome) &&
          (!workKind || run.work_kind === workKind) &&
          (!node || run.node === node) &&
          (!repo || run.repo === repo)
        );
      }),
    [allRuns, node, outcome, query, repo, workKind],
  );

  const filteredRaw = useMemo(
    () =>
      allRaw.filter((row) => {
        const rawNode = fieldString(row.fields, "node") || fieldString(row.fields, "target");
        const rawKind = fieldString(row.fields, "work_kind");
        const haystack = [row.kind, row.repo, row.id, row.summary, JSON.stringify(row.fields)]
          .join(" ")
          .toLowerCase();
        return (
          (!query || haystack.includes(query)) &&
          (!outcome || eventOutcome(row) === outcome) &&
          (!workKind || rawKind === workKind) &&
          (!node || rawNode === node) &&
          (!repo || row.repo === repo)
        );
      }),
    [allRaw, node, outcome, query, repo, workKind],
  );

  const selected = filteredRuns.find((run) => run.key === selectedKey) ?? filteredRuns[0];
  const outcomes = [...new Set(allRuns.map((run) => run.outcome))].sort();
  const workKinds = [...new Set(allRuns.map((run) => run.work_kind).filter(Boolean))].sort();
  const nodes = [...new Set(allRuns.map((run) => run.node).filter(Boolean))].sort();
  const repos = [...new Set(allRuns.map((run) => run.repo).filter(Boolean))].sort();
  const inFlight = allRuns.filter((run) => run.outcome === "in-flight").length;
  const failures = allRuns.filter((run) => FAILURE_OUTCOMES.has(run.outcome)).length;
  const held = allRuns.filter((run) => HELD_OUTCOMES.has(run.outcome)).length;

  const exportVisible = () => {
    if (presentation === "runs") {
      downloadCsv(`queue-runs-${since}.csv`, [
        ["started", "repo", "id", "work", "kind", "node", "outcome", "duration_secs", "task"],
        ...filteredRuns.map((run) => [
          new Date(run.started_at * 1000).toISOString(),
          run.repo,
          run.id,
          run.work,
          run.work_kind,
          run.node,
          run.outcome,
          run.duration_secs,
          run.task,
        ]),
      ]);
    } else {
      downloadCsv(`queue-events-${since}.csv`, [
        ["at", "kind", "repo", "id", "summary", "fields"],
        ...filteredRaw.map((row) => [
          new Date(row.at * 1000).toISOString(),
          row.kind,
          row.repo ?? "",
          row.id ?? "",
          row.summary,
          row.fields,
        ]),
      ]);
    }
  };

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-4">
      <header className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <p className="font-mono text-[10px] uppercase tracking-[0.14em] text-muted-foreground">
            Operational history
          </p>
          <h1 className="text-xl font-semibold">Queue events</h1>
          <p className="text-sm text-muted-foreground">
            Select any run to inspect its transitions, route, output, and durable artifacts.
          </p>
        </div>
        <div className="flex flex-wrap gap-2">
          <Button asChild size="sm" variant="secondary">
            <Link to="/queue">Now</Link>
          </Button>
          <Button
            size="sm"
            variant="secondary"
            disabled={
              events.isLoading ||
              (presentation === "runs" ? filteredRuns.length === 0 : filteredRaw.length === 0)
            }
            onClick={exportVisible}
          >
            <Download />
            Export visible rows
          </Button>
        </div>
      </header>

      <div className="grid overflow-hidden rounded-lg border border-border bg-border sm:grid-cols-2 lg:grid-cols-5">
        <SummaryMetric label={`Runs in ${since}`} value={events.isLoading ? "—" : allRuns.length} />
        <SummaryMetric
          label="In flight"
          value={inFlight}
          tone={inFlight > 0 ? "text-info" : undefined}
        />
        <SummaryMetric
          label="Failed or expired"
          value={failures}
          tone={failures > 0 ? "text-urgent" : undefined}
        />
        <SummaryMetric
          label="Skipped or held"
          value={held}
          tone={held > 0 ? "text-warn" : undefined}
        />
        <SummaryMetric label="Computer nodes" value={nodes.length} />
      </div>

      <div className="flex flex-wrap items-center gap-2">
        <div className="flex gap-1" role="group" aria-label="Time window">
          {WINDOWS.map((window) => (
            <Button
              key={window}
              size="sm"
              variant={since === window ? "default" : "secondary"}
              aria-pressed={since === window}
              onClick={() => setSince(window)}
            >
              {window}
            </Button>
          ))}
        </div>
        <div className="flex gap-1" role="group" aria-label="Event presentation">
          <Button
            size="sm"
            variant={presentation === "runs" ? "default" : "secondary"}
            aria-pressed={presentation === "runs"}
            onClick={() => setPresentation("runs")}
          >
            Runs
          </Button>
          <Button
            size="sm"
            variant={presentation === "raw" ? "default" : "secondary"}
            aria-pressed={presentation === "raw"}
            onClick={() => setPresentation("raw")}
          >
            Raw events
          </Button>
        </div>
        <label className="relative min-w-64 flex-1">
          <Search className="pointer-events-none absolute left-2.5 top-2 size-4 text-muted-foreground" />
          <Input
            value={search}
            onChange={(event) => setSearch(event.target.value)}
            placeholder="search work, repo, summary, run id, or task"
            aria-label="Search queue history"
            className="h-8 pl-8"
          />
        </label>
        <select
          value={outcome}
          onChange={(event) => setOutcome(event.target.value)}
          className="h-8 rounded-md border border-input bg-background px-2 text-sm"
          aria-label="Filter by outcome"
        >
          <option value="">All outcomes</option>
          {outcomes.map((value) => (
            <option key={value} value={value}>
              {value}
            </option>
          ))}
        </select>
        <select
          value={workKind}
          onChange={(event) => setWorkKind(event.target.value)}
          className="h-8 rounded-md border border-input bg-background px-2 text-sm"
          aria-label="Filter by work kind"
        >
          <option value="">All kinds</option>
          {workKinds.map((value) => (
            <option key={value} value={value}>
              {value}
            </option>
          ))}
        </select>
        <select
          value={node}
          onChange={(event) => setNode(event.target.value)}
          className="h-8 rounded-md border border-input bg-background px-2 text-sm"
          aria-label="Filter by node"
        >
          <option value="">All nodes</option>
          {nodes.map((value) => (
            <option key={value} value={value}>
              {value}
            </option>
          ))}
        </select>
        <select
          value={repo}
          onChange={(event) => setRepo(event.target.value)}
          className="h-8 rounded-md border border-input bg-background px-2 text-sm"
          aria-label="Filter by repo"
        >
          <option value="">All repos</option>
          {repos.map((value) => (
            <option key={value} value={value}>
              {value}
            </option>
          ))}
        </select>
      </div>

      {events.isError ? (
        <SectionCard title="Queue history">
          <LoadError>Could not read the queue event log.</LoadError>
        </SectionCard>
      ) : presentation === "runs" ? (
        <>
          <SectionCard
            eyebrow="paired evidence"
            title="Runs"
            count={events.isLoading ? undefined : `${filteredRuns.length} visible`}
          >
            {events.isLoading ? (
              <RowsSkeleton rows={6} />
            ) : allRuns.length === 0 ? (
              <Empty>
                No paired or actionable runs were recorded in this window. Raw events may still
                contain dispatcher pass details; widen the window or wait for new work.{" "}
                <Play className="inline size-3" />
              </Empty>
            ) : (
              <RunTable rows={filteredRuns} selectedKey={selected?.key} onSelect={setSelectedKey} />
            )}
          </SectionCard>
          <RunInspector
            run={selected}
            onShowRaw={(run) => {
              setPresentation("raw");
              setSearch(run.id || run.key);
              setOutcome("");
              setWorkKind("");
              setNode("");
              setRepo("");
            }}
          />
        </>
      ) : (
        <SectionCard
          eyebrow="append-only log"
          title="Raw events"
          count={events.isLoading ? undefined : `${filteredRaw.length} visible`}
        >
          {events.isLoading ? <RowsSkeleton rows={6} /> : <RawEvents rows={filteredRaw} />}
        </SectionCard>
      )}

      {failures > 0 ? (
        <p className="flex items-center gap-2 text-xs text-urgent">
          <AlertTriangle className="size-3.5" />
          {failures} run{failures === 1 ? "" : "s"} failed, expired, or lost an executor in this
          window.
        </p>
      ) : inFlight > 0 ? (
        <p className="flex items-center gap-2 text-xs text-info">
          <CircleDot className="size-3.5 animate-pulse" />
          {inFlight} run{inFlight === 1 ? " is" : "s are"} still in flight.
        </p>
      ) : null}
    </div>
  );
}
