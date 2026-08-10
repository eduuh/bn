import { useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { ArrowUpRight, Inbox, Plus, Search } from "lucide-react";
import { attnQuery, orchestrateQuery } from "../api/client";
import type { AttnItem } from "../api/types";
import { apiDispatch, createTask } from "../api/writes";
import { useAppMutation } from "../hooks/useAppMutation";
import { affects } from "../lib/invalidation";
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
import { PageHead } from "../components/PageHead";
import { RepoSelect } from "../components/RepoSelect";

type Harness = "claude" | "copilot";

/**
 * One research ask/finding, with a **Create task** that drops it into the enhance queue — so a
 * scout's findings become work rather than staying notes on a branch (#993). The task is created
 * with the finding's title and repo; the enhance agent then scopes it toward the SMART gate.
 */
function AskCard({ it }: { it: AttnItem }) {
  const make = useAppMutation({
    mutationFn: () => createTask({ title: it.title, repo: it.repo ?? undefined }),
    invalidates: affects.task,
    toast: {
      success: (r) => `Created ${r.id} — the enhance agent will scope it`,
      error: "Could not create task",
    },
  });
  return (
    <Card className="space-y-2 p-3">
      <div className="flex items-start gap-2">
        <span className="min-w-0 flex-1 font-medium leading-tight text-foreground">{it.title}</span>
        <span className="shrink-0 font-mono text-xs text-muted-foreground">{it.source}</span>
      </div>
      {it.repo && (
        <div className="font-mono text-xs text-muted-foreground">
          {it.repo}
          {it.branch ? `/${it.branch}` : ""}
        </div>
      )}
      {it.detail && (
        <pre className="max-h-48 overflow-auto rounded-md border border-border bg-muted/60 p-2 font-mono text-xs text-muted-foreground">
          {it.detail}
        </pre>
      )}
      <div className="flex items-center gap-3 pt-0.5">
        <Button
          size="sm"
          variant="secondary"
          disabled={make.isPending}
          onClick={() => make.mutate()}
          title="create a task from this finding — it lands in the enhance queue"
        >
          <Plus className="h-4 w-4" /> Create task
        </Button>
        {it.link && (
          <a
            href={it.link}
            target="_blank"
            rel="noreferrer"
            className="inline-flex items-center gap-0.5 text-sm text-primary"
          >
            open <ArrowUpRight className="h-3.5 w-3.5" />
          </a>
        )}
      </div>
    </Card>
  );
}

/**
 * `/research` — start a **research agent** (#483, ties to #378). A first-class, read-only *scout*:
 * distinct from the ship "Start agent", it investigates a question and returns its findings to the
 * branch note (and raises an ask on the attention list when it needs you) rather than opening a PR.
 * Dispatches `POST /api/dispatch {kind:"scout"}`; a paired token authorizes the write. Recent
 * agent-raised asks/answers surface below so you can read what came back.
 */
export function Research() {
  const orch = useQuery(orchestrateQuery());
  const attn = useQuery(attnQuery());
  const navigate = useNavigate();

  const runs = orch.data?.runs ?? [];
  const defaultRepo = runs[0]?.repo ?? "";

  const [question, setQuestion] = useState("");
  const [repo, setRepo] = useState(defaultRepo);
  const [harness, setHarness] = useState<Harness>("claude");

  const repoTouched = useRef(false);
  useEffect(() => {
    if (!repoTouched.current && !repo && defaultRepo) setRepo(defaultRepo);
  }, [defaultRepo, repo]);

  const q = question.trim();
  const canSubmit = q.length > 0 && repo.trim().length > 0;

  // This one navigates straight to /queue, so reconciling the fleet is the difference between
  // landing on the new agent and landing on a list that predates it.
  const research = useAppMutation({
    mutationFn: () => apiDispatch({ repo: repo.trim(), item: q, harness, kind: "scout" }),
    invalidates: affects.agentLaunch,
    toast: {
      success: "Research agent dispatched — findings land on the branch note",
      error: "Could not start research",
    },
    onSuccess: () => {
      setQuestion("");
      navigate("/queue");
    },
  });

  // Agent-raised attention items are the scout asks/answers coming back (source ≠ a manual raise).
  const asks = (attn.data?.items ?? []).filter((i) => i.source !== "manual");

  return (
    <div className="space-y-5 pb-8">
      <PageHead title="Research" sub="read-only scout — no PR, just findings" />

      <Card className="space-y-4 p-4">
        <div className="space-y-1.5">
          <Label htmlFor="research-q" className="flex items-baseline gap-2">
            Question
            <span className="text-xs font-normal text-muted-foreground/80">
              what should the scout find out?
            </span>
          </Label>
          <Textarea
            id="research-q"
            className="min-h-[6rem] font-mono text-[13px] leading-relaxed"
            rows={3}
            placeholder={
              "e.g. why is the nvim status_spec flaky?\nwhich crate owns the cron registry?"
            }
            value={question}
            onChange={(e) => setQuestion(e.target.value)}
          />
        </div>

        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <div className="space-y-1.5">
            <Label htmlFor="research-repo" className="flex items-baseline gap-2">
              Repo
              <span className="text-xs font-normal text-muted-foreground/80">required</span>
            </Label>
            <RepoSelect
              id="research-repo"
              value={repo}
              required
              onChange={(v) => {
                repoTouched.current = true;
                setRepo(v);
              }}
            />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="research-harness">Harness</Label>
            <Select value={harness} onValueChange={(v) => setHarness(v as Harness)}>
              <SelectTrigger id="research-harness">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="claude">claude</SelectItem>
                <SelectItem value="copilot">copilot</SelectItem>
              </SelectContent>
            </Select>
          </div>
        </div>

        <Button
          type="button"
          className="w-full"
          onClick={() => canSubmit && !research.isPending && research.mutate()}
          disabled={research.isPending || !canSubmit}
        >
          <Search className="h-4 w-4" />
          Start research agent
        </Button>
        <p className="text-xs text-muted-foreground">
          Runs read-only (a scout): it writes what it learns to the branch note and raises an ask
          here if it needs you. Watch it on the Fleet.
        </p>
      </Card>

      <section className="space-y-3">
        <div className="flex items-center gap-2">
          <h3 className="text-sm font-semibold uppercase tracking-wide text-muted-foreground">
            Recent asks &amp; findings
          </h3>
          <span className="text-xs text-muted-foreground">{asks.length}</span>
        </div>
        {attn.isLoading ? (
          <div className="text-sm text-muted-foreground">Loading research asks…</div>
        ) : asks.length === 0 ? (
          <Card className="flex flex-col items-center gap-2 px-6 py-10 text-center">
            <Inbox className="h-8 w-8 text-muted-foreground" />
            <p className="max-w-sm text-sm text-muted-foreground">
              No agent-raised research asks right now. Findings from a scout appear here and on its
              branch note.
            </p>
          </Card>
        ) : (
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            {asks.map((it) => (
              <AskCard key={it.id} it={it} />
            ))}
          </div>
        )}
      </section>

      <ToastStack />
    </div>
  );
}
