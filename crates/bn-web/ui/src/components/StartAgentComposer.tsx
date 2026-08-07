import { useState } from "react";
import { ArrowUpRight, Rocket, Plus, Zap } from "lucide-react";
import type { ActionItem, Urgency } from "../api/types";
import { apiDispatch, apiQueueAdd } from "../api/writes";
import { useAppMutation } from "../hooks/useAppMutation";
import { affects } from "../lib/invalidation";
import { dropAction } from "../lib/optimistic";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from "./ui/dialog";
import { Badge, type BadgeProps } from "./ui/badge";
import { Button } from "./ui/button";
import { Label } from "./ui/label";
import { Textarea } from "./ui/textarea";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "./ui/select";
import { cn } from "../lib/utils";
import { RepoSelect } from "./RepoSelect";

type Harness = "claude" | "copilot";
type Kind = "ship" | "scout";

/** Pull a string field out of an action's loosely-typed `payload`. */
function payloadStr(item: ActionItem, key: string): string {
  const v = item.payload?.[key];
  return typeof v === "string" ? v : "";
}

/** A segmented two-option toggle (urgency / kind) built from buttons. */
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
 * The Start-agent composer (#522) — the pivot from *decision* to *action*. Opened from any
 * command-center row, it pre-fills the editable textarea with that item's per-type
 * `suggested_prompt`, so the owner tweaks a real starting-point instruction rather than facing a
 * blank box. **Urgent** dispatches now (`POST /api/dispatch`); **Queue** enqueues for the next
 * orchestrate tick (`POST /api/orchestrate/queue`). The repo is seeded from the item's payload and
 * stays editable (some items — a goal, a global attention item — carry none, so the owner picks).
 */
export function StartAgentComposer({ item, onClose }: { item: ActionItem; onClose: () => void }) {
  const [urgency, setUrgency] = useState<Urgency>(item.default_urgency);
  const [prompt, setPrompt] = useState(item.suggested_prompt);
  const [repo, setRepo] = useState(payloadStr(item, "repo") || item.repo || "");
  const [harness, setHarness] = useState<Harness>("claude");
  const [kind, setKind] = useState<Kind>(item.type === "goal" ? "scout" : "ship");

  const start = useAppMutation({
    mutationFn: async () => {
      const body = prompt.trim();
      const r = repo.trim();
      if (urgency === "urgent") {
        await apiDispatch({ repo: r, item: body, harness, kind });
      } else {
        await apiQueueAdd({ repo: r || undefined, items: [body] });
      }
    },
    // Acting on the row resolves it, so it leaves the command centre with the composer.
    optimistic: dropAction(item.id),
    invalidates: affects.agentLaunch,
    toast: {
      success: urgency === "urgent" ? "Dispatched an agent" : "Queued for the next tick",
      error: "Could not start",
    },
    onSuccess: onClose,
  });

  const needsRepo = urgency === "urgent";
  const canStart =
    prompt.trim().length > 0 && (!needsRepo || repo.trim().length > 0) && !start.isPending;

  const kindVariant: BadgeProps["variant"] =
    item.severity === "urgent" ? "urgent" : item.severity === "warn" ? "warn" : "info";

  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent
        className="max-h-[92vh] max-w-lg gap-4 overflow-y-auto"
        onOpenAutoFocus={(e) => {
          // Focus the prompt (not the first button) so the owner can edit immediately.
          e.preventDefault();
          const el = document.getElementById("composer-prompt") as HTMLTextAreaElement | null;
          el?.focus();
        }}
      >
        <DialogHeader>
          <div className="flex flex-wrap items-center gap-2 pr-6">
            <Badge variant={kindVariant} className="uppercase tracking-wide">
              {item.type}
            </Badge>
            {item.link && (
              <a
                href={String(item.link)}
                target="_blank"
                rel="noreferrer"
                className="inline-flex items-center gap-0.5 text-xs text-primary"
              >
                open <ArrowUpRight className="h-3 w-3" />
              </a>
            )}
          </div>
          <DialogTitle>{item.title}</DialogTitle>
          <DialogDescription className="text-xs">{item.source}</DialogDescription>
        </DialogHeader>

        <div
          className="grid grid-cols-2 gap-1 rounded-lg bg-muted p-1"
          role="group"
          aria-label="urgency"
        >
          <Segment
            active={urgency === "urgent"}
            onClick={() => setUrgency("urgent")}
            title="dispatch a new agent immediately"
          >
            <Zap className="h-4 w-4" /> Urgent — now
          </Segment>
          <Segment
            active={urgency === "queued"}
            onClick={() => setUrgency("queued")}
            title="add to the orchestrate queue for the next tick"
          >
            <Plus className="h-4 w-4" /> Queue
          </Segment>
        </div>

        <div className="space-y-1.5">
          <Label htmlFor="composer-prompt" className="flex items-baseline gap-2">
            Prompt for the agent
            <span className="text-xs font-normal text-muted-foreground/80">
              editable — a starting point from you
            </span>
          </Label>
          <Textarea
            id="composer-prompt"
            className="min-h-[9rem] font-mono text-[13px] leading-relaxed"
            value={prompt}
            onChange={(e) => setPrompt(e.target.value)}
          />
        </div>

        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <div className="space-y-1.5">
            <Label htmlFor="composer-repo" className="flex items-baseline gap-2">
              Repo
              {needsRepo && (
                <span className="text-xs font-normal text-muted-foreground/80">required</span>
              )}
            </Label>
            <RepoSelect id="composer-repo" value={repo} required={needsRepo} onChange={setRepo} />
          </div>
          {urgency === "urgent" && (
            <div className="space-y-1.5">
              <Label htmlFor="composer-harness">Harness</Label>
              <Select value={harness} onValueChange={(v) => setHarness(v as Harness)}>
                <SelectTrigger id="composer-harness">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="claude">claude</SelectItem>
                  <SelectItem value="copilot">copilot</SelectItem>
                </SelectContent>
              </Select>
            </div>
          )}
        </div>

        {urgency === "urgent" && (
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
                title="a read-only scout (research / answer)"
              >
                scout
              </Segment>
            </div>
          </div>
        )}

        <DialogFooter>
          <Button variant="ghost" onClick={onClose} disabled={start.isPending}>
            Cancel
          </Button>
          <Button onClick={() => start.mutate()} disabled={!canStart}>
            {urgency === "urgent" ? (
              <>
                <Rocket className="h-4 w-4" /> Dispatch agent
              </>
            ) : (
              <>
                <Plus className="h-4 w-4" /> Add to queue
              </>
            )}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
