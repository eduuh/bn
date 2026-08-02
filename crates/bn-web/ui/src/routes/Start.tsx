import { useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import { useMutation, useQuery } from "@tanstack/react-query";
import { Minus, Plus, Rocket } from "lucide-react";
import { fleetQuery, orchestrateQuery } from "../api/client";
import { apiDispatch, apiQueueAdd, apiStart } from "../api/writes";
import { parseItems } from "../lib/backlog";
import {
  defaultRepo as computeDefaultRepo,
  harnessAvailable,
  harnessReason,
  knownRepos,
  runningHarness,
  type Harness,
} from "../lib/start";
import { pushToast } from "../lib/toast";
import { ToastStack } from "../components/ToastStack";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
import { Input } from "../components/ui/input";
import { Label } from "../components/ui/label";
import { Textarea } from "../components/ui/textarea";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "../components/ui/select";
import { cn } from "../lib/utils";
import { PageHead } from "../components/PageHead";

type Mode = "dispatch" | "queue" | "daemon";
type Kind = "ship" | "scout";

const HARNESS_OPTIONS: Harness[] = ["claude", "copilot"];

const MODES: { value: Mode; label: string }[] = [
  { value: "dispatch", label: "Dispatch now" },
  { value: "queue", label: "Add to queue" },
  { value: "daemon", label: "Daemon run" },
];

/** A segmented toggle option (mode / kind) built from a button, matching the Phase-1 composer. */
function Segment({
  active,
  onClick,
  title,
  children,
}: {
  active: boolean;
  onClick: () => void;
  title?: string;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={title}
      aria-pressed={active}
      className={cn(
        "inline-flex flex-1 items-center justify-center gap-1.5 rounded-md px-3 py-2 text-sm font-medium transition-colors",
        active
          ? "bg-card text-foreground shadow-card"
          : "text-muted-foreground hover:text-foreground",
      )}
    >
      {children}
    </button>
  );
}

/** A clamped −/value/+ integer stepper (concurrency lanes). */
function Stepper({
  value,
  min = 1,
  max = 32,
  disabled = false,
  onChange,
  label,
}: {
  value: number;
  min?: number;
  max?: number;
  disabled?: boolean;
  onChange: (v: number) => void;
  label?: string;
}) {
  const set = (v: number) => {
    const next = Math.max(min, Math.min(max, v));
    if (next !== value) onChange(next);
  };
  return (
    <div
      role="group"
      aria-label={label ?? "stepper"}
      className="inline-flex items-center gap-1 rounded-lg bg-muted p-1"
    >
      <Button
        type="button"
        variant="ghost"
        size="icon"
        className="h-8 w-8"
        disabled={disabled || value <= min}
        onClick={() => set(value - 1)}
        aria-label="decrease"
      >
        <Minus className="h-4 w-4" />
      </Button>
      <span className="min-w-8 text-center font-mono text-sm tabular-nums text-foreground">
        {value}
      </span>
      <Button
        type="button"
        variant="ghost"
        size="icon"
        className="h-8 w-8"
        disabled={disabled || value >= max}
        onClick={() => set(value + 1)}
        aria-label="increase"
      >
        <Plus className="h-4 w-4" />
      </Button>
    </div>
  );
}

/**
 * `/start` — the first-class intake (design §479, job 2: *Act*). Starting an agent is THE primary
 * action of the dashboard, so it gets its own prominent screen: one big form (task · repo · harness
 * · ship/scout) whose primary button dispatches a real agent (`POST /api/dispatch`), plus secondary
 * modes to queue for the daemon or start a daemon run. A paired web token authorizes the write;
 * success navigates to the Fleet so you watch it come alive.
 */
export function Start() {
  const orch = useQuery(orchestrateQuery());
  const fleet = useQuery(fleetQuery());
  const navigate = useNavigate();

  const runs = orch.data?.runs ?? [];
  const repoOptions = knownRepos(runs, fleet.data);
  const defaultRepo = computeDefaultRepo(runs, fleet.data);
  const runningHarnessName = runningHarness(fleet.data);

  const [mode, setMode] = useState<Mode>("dispatch");
  const [text, setText] = useState("");
  const [repo, setRepo] = useState(defaultRepo);
  const [harness, setHarness] = useState<Harness>("claude");
  const [kind, setKind] = useState<Kind>("ship");
  const [concurrency, setConcurrency] = useState(2);

  // Adopt the sole run's / live-fleet's repo once data hydrates, unless the user has already typed one.
  const repoTouched = useRef(false);
  useEffect(() => {
    if (!repoTouched.current && !repo && defaultRepo) setRepo(defaultRepo);
  }, [defaultRepo, repo]);
  const onRepo = (v: string) => {
    repoTouched.current = true;
    setRepo(v);
  };

  // Default the harness selector to the one already in flight (a live agent's harness), unless the
  // user has picked one — so Start mirrors reality instead of a fixed guess.
  const harnessTouched = useRef(false);
  useEffect(() => {
    if (!harnessTouched.current && runningHarnessName) setHarness(runningHarnessName);
  }, [runningHarnessName]);
  const onHarness = (v: Harness) => {
    harnessTouched.current = true;
    setHarness(v);
  };

  const items = parseItems(text);
  const badItem = items.find((i) => i.startsWith("-"));
  const needsRepo = mode === "dispatch";
  const canSubmit = items.length > 0 && !badItem && (!needsRepo || repo.trim().length > 0);

  const onError = (e: unknown) => {
    pushToast(e instanceof Error ? e.message : "could not start", "error");
  };
  const onStarted = (n: number, verb: string) => {
    pushToast(`${verb} ${n} agent${n === 1 ? "" : "s"} — watch the fleet`, "success");
    setText("");
    navigate("/fleet");
  };

  const dispatchMut = useMutation({
    mutationFn: async () => {
      const r = repo.trim();
      await Promise.all(items.map((item) => apiDispatch({ repo: r, item, harness, kind })));
      return items.length;
    },
    onSuccess: (n) => onStarted(n, "Dispatched"),
    onError,
  });
  const queueMut = useMutation({
    mutationFn: () => apiQueueAdd({ repo: repo.trim() || undefined, items }),
    onSuccess: () => onStarted(items.length, "Queued"),
    onError,
  });
  const daemonMut = useMutation({
    mutationFn: () => apiStart({ repo: repo.trim() || undefined, items, concurrency }),
    onSuccess: () => onStarted(items.length, "Started a run of"),
    onError,
  });

  const busy = dispatchMut.isPending || queueMut.isPending || daemonMut.isPending;
  const submit = () => {
    if (!canSubmit || busy) return;
    if (mode === "dispatch") dispatchMut.mutate();
    else if (mode === "queue") queueMut.mutate();
    else daemonMut.mutate();
  };

  const primaryLabel =
    mode === "dispatch"
      ? `Start agent${items.length > 1 ? ` ×${items.length}` : ""}`
      : mode === "queue"
        ? `Add to queue${items.length > 1 ? ` (${items.length})` : ""}`
        : `Start daemon run${items.length > 1 ? ` (${items.length})` : ""}`;

  const hint =
    mode === "dispatch"
      ? "Dispatches immediately, skipping the queue — appears on the Fleet within a tick."
      : mode === "queue"
        ? "Appends to the running daemon's queue (start a daemon run first if none is live)."
        : "Starts a launchd-daemonized run that works the queue at the chosen concurrency.";

  return (
    <div className="mx-auto max-w-2xl space-y-5 pb-8">
      <PageHead title="Start an agent" sub="dispatch · queue · daemon run" />

      <Card className="space-y-4 p-4">
        <div
          className="grid grid-cols-3 gap-1 rounded-lg bg-muted p-1"
          role="group"
          aria-label="start mode"
        >
          {MODES.map((m) => (
            <Segment key={m.value} active={mode === m.value} onClick={() => setMode(m.value)}>
              {m.label}
            </Segment>
          ))}
        </div>

        <div className="space-y-1.5">
          <Label htmlFor="start-item" className="flex flex-wrap items-baseline gap-2">
            Task
            <span className="text-xs font-normal text-muted-foreground/80">
              issue # / URL / free text — one per line
            </span>
          </Label>
          <Textarea
            id="start-item"
            className="min-h-[6rem] font-mono text-[13px] leading-relaxed"
            rows={3}
            placeholder={"#479\nhttps://github.com/eduuh/bn/issues/480\ninvestigate flaky nvim status_spec"}
            value={text}
            onChange={(e) => setText(e.target.value)}
          />
        </div>

        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <div className="space-y-1.5">
            <Label htmlFor="start-repo" className="flex items-baseline gap-2">
              Repo
              {needsRepo && (
                <span className="text-xs font-normal text-muted-foreground/80">required</span>
              )}
            </Label>
            <Input
              id="start-repo"
              className="font-mono"
              list={repoOptions.length ? "start-repo-list" : undefined}
              value={repo}
              placeholder={needsRepo ? "repo (required)" : "repo (optional)"}
              autoComplete="off"
              spellCheck={false}
              onChange={(e) => onRepo(e.target.value)}
            />
            {repoOptions.length > 0 && (
              <datalist id="start-repo-list">
                {repoOptions.map((r) => (
                  <option key={r} value={r} />
                ))}
              </datalist>
            )}
          </div>

          {mode === "daemon" && (
            <div className="space-y-1.5">
              <Label>Concurrency</Label>
              <Stepper
                value={concurrency}
                onChange={setConcurrency}
                disabled={busy}
                label="concurrency"
              />
            </div>
          )}
        </div>

        {mode === "dispatch" && (
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            <div className="space-y-1.5">
              <Label htmlFor="start-harness">Harness</Label>
              <Select value={harness} onValueChange={(v) => onHarness(v as Harness)}>
                <SelectTrigger id="start-harness">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {HARNESS_OPTIONS.map((h) => {
                    const available = harnessAvailable(h, fleet.data);
                    return (
                      <SelectItem
                        key={h}
                        value={h}
                        disabled={!available}
                        title={available ? undefined : harnessReason(h, fleet.data)}
                      >
                        {h}
                        {available ? "" : " (unavailable)"}
                      </SelectItem>
                    );
                  })}
                </SelectContent>
              </Select>
            </div>
            <div className="space-y-1.5">
              <Label>Kind</Label>
              <div
                className="grid grid-cols-2 gap-1 rounded-lg bg-muted p-1"
                role="group"
                aria-label="ship or scout"
              >
                <Segment
                  active={kind === "ship"}
                  onClick={() => setKind("ship")}
                  title="a full ship (build → PR)"
                >
                  ship
                </Segment>
                <Segment
                  active={kind === "scout"}
                  onClick={() => setKind("scout")}
                  title="a scout (research / delegate-for-answers)"
                >
                  scout
                </Segment>
              </div>
            </div>
          </div>
        )}

        {badItem && (
          <p className="text-sm text-urgent">Tasks can’t start with “-” ({badItem}).</p>
        )}

        <Button
          type="button"
          size="lg"
          className="w-full"
          onClick={submit}
          disabled={busy || !canSubmit}
        >
          <Rocket className="h-4 w-4" /> {primaryLabel}
        </Button>
        <p className="text-xs text-muted-foreground">{hint}</p>
      </Card>

      <ToastStack />
    </div>
  );
}
