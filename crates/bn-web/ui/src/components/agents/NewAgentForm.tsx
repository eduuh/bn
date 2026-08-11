import { useState } from "react";
import { Loader2, Plus, X } from "lucide-react";
import { AGENT_DEF_SCOPES, AGENT_KINDS } from "../../api/types";
import type { AgentDefScope, AgentKind } from "../../api/types";
import { createAgentDef } from "../../api/writes";
import { useAppMutation } from "../../hooks/useAppMutation";
import { affects } from "../../lib/invalidation";
import { pushToast } from "../../lib/toast";
import { Button } from "../ui/button";
import { Card } from "../ui/card";
import { Input } from "../ui/input";
import { Label } from "../ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "../ui/select";
import { Textarea } from "../ui/textarea";

/** What each kind is for, and — the part a form has to get right — which extra fields its validator
 * requires. Shown under the kind picker so the conditional inputs below have a reason. */
const KIND_HELP: Record<AgentKind, string> = {
  command: "Runs a bn command on a schedule or on demand. Needs nothing but a prompt.",
  staff: "Renders a briefing card from a gathered command. Needs `gather` and at least one sink.",
  hosted: "A prompt run in a hosted harness session. Needs a body.",
  specialist:
    "A repo-gated PR reviewer, compiled into a native Claude/Copilot agent. Needs a repo.",
};

/** The sinks a staff agent may write to — `bn_core::staff::SINKS`. Kept as a fixed list because the
 * server rejects anything outside it, and a free-text field would only discover that on submit. */
const SINKS = ["attn", "daily", "note", "dojo", "research"];

/**
 * The create form. Name + kind + tier + description, then whichever extra fields the chosen kind
 * actually requires — `gather`/`sinks` for staff, `repo` for a specialist.
 *
 * The conditional fields are the whole point. Every kind's validator both *requires* some fields
 * and *forbids* the other kinds' fields, so one flat form would either produce a `400` on the
 * common path or quietly write an agent that means something other than what was typed. Revealing
 * exactly the required set keeps the create honest without duplicating the rules.
 *
 * A specialist left with an empty body is scaffolded server-side into a complete reviewer — the
 * same file `bn agents new` writes, with its standards reference, `auto-review-pr` skill and
 * persona — rather than the stub this form could assemble.
 */
export function NewAgentForm({
  onDone,
  onCancel,
}: {
  onDone: (name: string) => void;
  onCancel: () => void;
}) {
  const [name, setName] = useState("");
  const [kind, setKind] = useState<AgentKind>("command");
  const [scope, setScope] = useState<AgentDefScope>("global-personal");
  const [description, setDescription] = useState("");
  const [gather, setGather] = useState("");
  const [sinks, setSinks] = useState<string[]>(["note"]);
  const [repo, setRepo] = useState("");
  const [body, setBody] = useState("");

  const create = useAppMutation({
    mutationFn: () =>
      createAgentDef({
        name: name.trim(),
        kind,
        description: description.trim(),
        scope,
        // Sent only for the kind that allows them: every other kind's validator *forbids* these,
        // so passing an empty string through would turn a valid create into a 400.
        gather: kind === "staff" ? gather.trim() : undefined,
        sinks: kind === "staff" ? sinks : undefined,
        repo: kind === "specialist" ? repo.trim() : undefined,
        body,
      }),
    invalidates: affects.agentDefs,
    toast: { error: "Create failed" },
    onSuccess: (result) => {
      pushToast(`Created agent “${name.trim()}” at ${result.agent.path}`, "success");
      onDone(name.trim());
    },
  });

  // The server applies the same rules; mirroring the cheap ones here keeps the button honest.
  const nameOk = /^[A-Za-z0-9_-]{1,64}$/.test(name.trim());
  const ready =
    nameOk &&
    description.trim().length > 0 &&
    (kind !== "staff" || (gather.trim().length > 0 && sinks.length > 0)) &&
    (kind !== "specialist" || repo.trim().length > 0) &&
    // Only a command may ship with an empty prompt; a specialist's empty body means "scaffold it".
    (kind === "command" || kind === "specialist" || body.trim().length > 0);

  const toggleSink = (sink: string) =>
    setSinks((prev) => (prev.includes(sink) ? prev.filter((s) => s !== sink) : [...prev, sink]));

  return (
    <Card className="space-y-3 p-4">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold text-foreground">New agent</h3>
        <Button variant="ghost" size="icon" aria-label="Cancel" onClick={onCancel}>
          <X className="h-4 w-4" />
        </Button>
      </div>

      <div className="grid gap-3 sm:grid-cols-3">
        <div className="space-y-1.5">
          <Label htmlFor="agent-name">Name</Label>
          <Input
            id="agent-name"
            value={name}
            spellCheck={false}
            placeholder="pr-sweep"
            onChange={(e) => setName(e.target.value)}
            className="font-mono"
          />
          {name.trim() !== "" && !nameOk && (
            <p className="text-xs text-urgent">letters, digits, “-” and “_” only (max 64)</p>
          )}
        </div>
        <div className="space-y-1.5">
          <Label htmlFor="agent-kind">Kind</Label>
          <Select value={kind} onValueChange={(v) => setKind(v as AgentKind)}>
            <SelectTrigger id="agent-kind">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {AGENT_KINDS.map((k) => (
                <SelectItem key={k} value={k}>
                  {k}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <div className="space-y-1.5">
          <Label htmlFor="agent-scope">Tier</Label>
          <Select value={scope} onValueChange={(v) => setScope(v as AgentDefScope)}>
            <SelectTrigger id="agent-scope">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {AGENT_DEF_SCOPES.map((s) => (
                <SelectItem key={s} value={s}>
                  {s}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      </div>

      <p className="text-xs text-muted-foreground">{KIND_HELP[kind]}</p>

      <div className="space-y-1.5">
        <Label htmlFor="agent-description">Description</Label>
        <Input
          id="agent-description"
          value={description}
          placeholder="What this agent does, in one line"
          onChange={(e) => setDescription(e.target.value)}
        />
      </div>

      {kind === "staff" && (
        <div className="grid gap-3 sm:grid-cols-2">
          <div className="space-y-1.5">
            <Label htmlFor="agent-gather">Gather</Label>
            <Input
              id="agent-gather"
              value={gather}
              spellCheck={false}
              className="font-mono"
              placeholder="bn status --json"
              onChange={(e) => setGather(e.target.value)}
            />
          </div>
          <div className="space-y-1.5">
            <Label>Sinks</Label>
            <div className="flex flex-wrap gap-2 pt-1">
              {SINKS.map((sink) => (
                <Button
                  key={sink}
                  type="button"
                  size="sm"
                  variant={sinks.includes(sink) ? "default" : "outline"}
                  aria-pressed={sinks.includes(sink)}
                  onClick={() => toggleSink(sink)}
                >
                  {sink}
                </Button>
              ))}
            </div>
          </div>
        </div>
      )}

      {kind === "specialist" && (
        <div className="space-y-1.5">
          <Label htmlFor="agent-repo">Repo</Label>
          <Input
            id="agent-repo"
            value={repo}
            spellCheck={false}
            className="font-mono"
            placeholder="owner/name"
            onChange={(e) => setRepo(e.target.value)}
          />
        </div>
      )}

      <div className="space-y-1.5">
        <Label htmlFor="agent-body">
          Prompt{" "}
          {kind === "specialist"
            ? "(optional — leave empty to scaffold a full reviewer)"
            : kind === "command"
              ? "(optional)"
              : ""}
        </Label>
        <Textarea
          id="agent-body"
          className="min-h-[10rem] font-mono text-[13px] leading-relaxed"
          value={body}
          spellCheck={false}
          placeholder="The markdown prompt this agent runs."
          onChange={(e) => setBody(e.target.value)}
          rows={8}
        />
      </div>

      <div className="flex items-center gap-3">
        <Button disabled={!ready || create.isPending} onClick={() => create.mutate()}>
          {create.isPending ? (
            <>
              <Loader2 className="h-4 w-4 animate-spin" /> Creating…
            </>
          ) : (
            <>
              <Plus className="h-4 w-4" /> Create agent
            </>
          )}
        </Button>
        {kind === "specialist" && (
          <span className="text-xs text-muted-foreground">
            also compiled into the Claude and Copilot agent roots
          </span>
        )}
      </div>
    </Card>
  );
}
