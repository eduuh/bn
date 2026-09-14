import { useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { Rocket } from "lucide-react";
import { fleetQuery, orchestrateQuery } from "../api/client";
import { apiDispatch } from "../api/writes";
import { parseItems } from "../lib/backlog";
import { useAppMutation } from "../hooks/useAppMutation";
import { affects } from "../lib/invalidation";
import {
  defaultRepo as computeDefaultRepo,
  harnessAvailable,
  harnessReason,
  runningHarness,
  type Harness,
} from "../lib/start";
import { ToastStack } from "../components/ToastStack";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
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
import { RepoSelect } from "../components/RepoSelect";

type Kind = "ship" | "scout";

const HARNESS_OPTIONS: Harness[] = ["claude", "copilot"];

/** A segmented toggle option (kind) built from a button, matching the Phase-1 composer. */
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

/**
 * `/start` — the first-class intake (design §479, job 2: *Act*). Starting an agent is THE primary
 * action of the dashboard, so it gets its own prominent screen: one big form (task · repo · harness
 * · ship/scout) whose primary button dispatches a real agent (`POST /api/dispatch`). A paired web
 * token authorizes the write; success navigates to the console so you watch it come alive.
 */
export function Start() {
  const orch = useQuery(orchestrateQuery());
  const fleet = useQuery(fleetQuery());
  const navigate = useNavigate();

  const runs = orch.data?.runs ?? [];
  const defaultRepo = computeDefaultRepo(runs, fleet.data);
  const runningHarnessName = runningHarness(fleet.data);

  const [text, setText] = useState("");
  const [repo, setRepo] = useState(defaultRepo);
  const [harness, setHarness] = useState<Harness>("claude");
  const [kind, setKind] = useState<Kind>("ship");

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
  const canSubmit = items.length > 0 && !badItem && repo.trim().length > 0;

  // Lands on the console, so the fleet has to be reconciled or you arrive at a list that predates
  // what you just launched.
  const dispatchMut = useAppMutation({
    mutationFn: async () => {
      const r = repo.trim();
      await Promise.all(items.map((item) => apiDispatch({ repo: r, item, harness, kind })));
      return items.length;
    },
    invalidates: affects.agentLaunch,
    onSuccess: () => {
      setText("");
      navigate("/console");
    },
    toast: {
      success: (n: number) => `Dispatched ${n} agent${n === 1 ? "" : "s"}`,
      error: "Could not start",
    },
  });

  const busy = dispatchMut.isPending;
  const submit = () => {
    if (!canSubmit || busy) return;
    dispatchMut.mutate();
  };

  const primaryLabel = `Start agent${items.length > 1 ? ` ×${items.length}` : ""}`;

  return (
    // No nested measure. This used to be `mx-auto max-w-2xl` *inside* the shell's own centred
    // container, which centred the form within the centred column and pushed this page's title
    // 170px right of the title on every other route. The shell now gives `/start` a `document`
    // width (see `lib/layout`), so the page just fills what it is given.
    <div className="space-y-5 pb-8">
      <PageHead title="Start an agent" sub="dispatch now" />

      <Card className="space-y-4 p-4">
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
            placeholder={
              "#479\nhttps://github.com/eduuh/bn/issues/480\ninvestigate flaky nvim status_spec"
            }
            value={text}
            onChange={(e) => setText(e.target.value)}
          />
        </div>

        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <div className="space-y-1.5">
            <Label htmlFor="start-repo" className="flex items-baseline gap-2">
              Repo
              <span className="text-xs font-normal text-muted-foreground/80">required</span>
            </Label>
            <RepoSelect id="start-repo" value={repo} onChange={onRepo} required />
          </div>
        </div>

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

        {badItem && <p className="text-sm text-urgent">Tasks can’t start with “-” ({badItem}).</p>}

        <Button
          type="button"
          size="lg"
          className="w-full"
          onClick={submit}
          disabled={busy || !canSubmit}
        >
          <Rocket className="h-4 w-4" /> {primaryLabel}
        </Button>
        <p className="text-xs text-muted-foreground">
          Dispatches immediately. The agent appears on the console within a tick.
        </p>
      </Card>

      <ToastStack />
    </div>
  );
}
