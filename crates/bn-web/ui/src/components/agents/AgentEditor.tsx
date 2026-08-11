import { useEffect, useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { AlertTriangle, Loader2, Save, Trash2 } from "lucide-react";
import { agentDefQuery, queryKeys } from "../../api/client";
import type { AgentDefMutation, AgentDefNativeSync } from "../../api/types";
import { deleteAgentDef, saveAgentDef } from "../../api/writes";
import { useAppMutation } from "../../hooks/useAppMutation";
import { affects } from "../../lib/invalidation";
import { pushToast } from "../../lib/toast";
import { Badge } from "../ui/badge";
import { Button } from "../ui/button";
import { Card } from "../ui/card";
import { Skeleton } from "../ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "../ui/tabs";
import { Textarea } from "../ui/textarea";
import { Markdown } from "../Markdown";
import { kindBadge, trigger } from "./agentMeta";

/** One line describing the Copilot/Claude native-agent re-install a specialist write triggers.
 * `null` is not a failure — it is the ordinary case, an agent that compiles nowhere. */
function nativeSummary(native: AgentDefNativeSync | null): string | null {
  if (!native) return null;
  if (!native.synced) return native.error ?? "harness sync failed";
  const { created = 0, updated = 0, removed = 0, harnesses } = native;
  const where = harnesses.length ? harnesses.join(" + ") : "no harness";
  return created + updated + removed === 0 ? `${where} already current` : `installed to ${where}`;
}

/** Toast a mutation, folding the harness sync into the same line. A source write that lands but
 * fails to reach Claude/Copilot is a warning, not a success. */
function reportMutation(message: string, result: AgentDefMutation) {
  const detail = nativeSummary(result.native);
  const failed = result.native != null && !result.native.synced;
  pushToast(detail ? `${message} — ${detail}` : message, failed ? "error" : "success");
}

/** One labelled fact in the metadata grid. Renders nothing when there is no value — an empty
 * "Model —" row is noise, not information. */
function Fact({ label, value }: { label: string; value?: string | null }) {
  if (!value) return null;
  return (
    <div className="min-w-0">
      <div className="text-[11px] uppercase tracking-wide text-muted-foreground/70">{label}</div>
      <div className="truncate font-mono text-[12.5px] text-foreground" title={value}>
        {value}
      </div>
    </div>
  );
}

/** Everything below the `---` fence. The preview renders the prompt, and a reader who wanted to
 * see the frontmatter is already looking at it on the Source tab. */
function promptBody(source: string): string {
  const match = /^---\r?\n[\s\S]*?\r?\n---\r?\n?/.exec(source);
  return match ? source.slice(match[0].length).trimStart() : source;
}

/**
 * The detail pane for one agent: what it is, then its `.md` — editable.
 *
 * The whole file is edited, frontmatter included, because for an agent the frontmatter *is* half
 * the definition: the schedule, the harness, the model and the declared arguments all live up
 * there, and an editor that hid them would only be able to change the prose.
 *
 * Server-validated rather than client-validated. `agents::discover` fails the entire catalog on one
 * unparseable file, so the rules are genuinely intricate (each kind requires some fields and
 * forbids others) and duplicating them here would mean two validators that drift. The server parses
 * the exact bytes it is about to write and returns the parse error verbatim; this shows it.
 */
export function AgentEditor({ name, onDeleted }: { name: string; onDeleted: () => void }) {
  const detail = useQuery(agentDefQuery(name));
  const [draft, setDraft] = useState("");
  const [dirty, setDirty] = useState(false);
  const [confirmingDelete, setConfirmingDelete] = useState(false);

  // Load the fetched source into the editable draft whenever a fresh agent loads.
  useEffect(() => {
    if (detail.data) {
      setDraft(detail.data.source);
      setDirty(false);
    }
  }, [detail.data]);

  // A different agent selected: drop any half-armed delete so it can't fire on the wrong one.
  useEffect(() => setConfirmingDelete(false), [name]);

  const save = useAppMutation({
    mutationFn: () => saveAgentDef(name, draft),
    invalidates: [...affects.agentDefs, queryKeys.agentDef(name)],
    // Refetching after a failed save would replace the unsaved draft from `detail.data` — which is
    // exactly the text the author now needs in front of them to fix what the server rejected.
    invalidateOnError: false,
    toast: { error: "Save failed" },
    onSuccess: (result) => {
      reportMutation(`Saved agent “${name}”`, result);
      setDirty(false);
    },
  });

  const remove = useAppMutation({
    mutationFn: () => deleteAgentDef(name),
    invalidates: [...affects.agentDefs, queryKeys.agentDef(name)],
    invalidateOnError: false,
    toast: { error: "Delete failed" },
    onSuccess: (result) => {
      // Deleting the effective definition unshadows a lower tier when one exists — say which,
      // rather than implying the name is gone.
      const fate = result.remaining_scope
        ? `now served from ${result.remaining_scope}`
        : "removed from every tier";
      reportMutation(`Deleted agent “${name}” (${fate})`, result);
      onDeleted();
    },
    onError: () => setConfirmingDelete(false),
  });

  const preview = useMemo(() => promptBody(draft), [draft]);

  if (detail.isLoading) {
    return (
      <div className="space-y-3" aria-hidden="true">
        <Skeleton className="h-5 w-40" />
        <Skeleton className="h-3 w-2/3" />
        <Skeleton className="h-64 w-full" />
      </div>
    );
  }
  if (detail.isError) {
    return (
      <Card className="flex items-center gap-2 border-urgent/30 bg-urgent-muted p-4 text-sm text-urgent">
        <AlertTriangle className="h-4 w-4" />
        Could not load “{name}”.
      </Card>
    );
  }

  const a = detail.data?.agent;
  return (
    <div className="space-y-4">
      <div className="space-y-2">
        <div className="flex flex-wrap items-center gap-2">
          <h3 className="font-mono text-sm font-semibold text-foreground">{name}</h3>
          {a && kindBadge(a)}
          {a && <Badge variant="muted">{a.scope}</Badge>}
        </div>
        {a?.description && <p className="text-sm text-muted-foreground">{a.description}</p>}
      </div>

      {a && (
        <div className="grid grid-cols-2 gap-x-4 gap-y-3 sm:grid-cols-3">
          <Fact label="Trigger" value={trigger(a)} />
          <Fact label="Harness" value={a.harness} />
          <Fact label="Model" value={a.model} />
          <Fact label="Runs" value={a.run} />
          <Fact label="Gathers" value={a.gather} />
          <Fact label="Repo" value={a.repo} />
          <Fact label="Skills" value={a.skills.length ? a.skills.join(", ") : null} />
          <Fact label="MCPs" value={a.mcps.length ? a.mcps.join(", ") : null} />
          <Fact
            label="Arguments"
            value={
              a.arguments.length
                ? a.arguments.map((x) => (x.required ? x.name : `${x.name}?`)).join(", ")
                : null
            }
          />
        </div>
      )}

      <Tabs defaultValue="source">
        <TabsList>
          <TabsTrigger value="source">Source</TabsTrigger>
          <TabsTrigger value="preview">Preview</TabsTrigger>
        </TabsList>

        <TabsContent value="source" className="space-y-3">
          <Textarea
            aria-label={`Edit ${name} agent`}
            className="min-h-[26rem] font-mono text-[13px] leading-relaxed"
            value={draft}
            spellCheck={false}
            onChange={(e) => {
              setDraft(e.target.value);
              setDirty(true);
            }}
            rows={24}
          />
          <div className="flex flex-wrap items-center gap-3">
            <Button disabled={save.isPending || !dirty} onClick={() => save.mutate()}>
              {save.isPending ? (
                <>
                  <Loader2 className="h-4 w-4 animate-spin" /> Saving…
                </>
              ) : (
                <>
                  <Save className="h-4 w-4" /> Save agent
                </>
              )}
            </Button>
            {dirty && <span className="text-xs text-muted-foreground">unsaved changes</span>}

            <div className="ml-auto flex items-center gap-2">
              {confirmingDelete && (
                <span className="text-xs text-urgent">Delete this file permanently?</span>
              )}
              <Button
                variant={confirmingDelete ? "destructive" : "outline"}
                disabled={remove.isPending}
                onClick={() => (confirmingDelete ? remove.mutate() : setConfirmingDelete(true))}
              >
                {remove.isPending ? (
                  <>
                    <Loader2 className="h-4 w-4 animate-spin" /> Deleting…
                  </>
                ) : (
                  <>
                    <Trash2 className="h-4 w-4" /> {confirmingDelete ? "Confirm delete" : "Delete"}
                  </>
                )}
              </Button>
              {confirmingDelete && !remove.isPending && (
                <Button variant="ghost" onClick={() => setConfirmingDelete(false)}>
                  Cancel
                </Button>
              )}
            </div>
          </div>
        </TabsContent>

        {/* The prompt as the agent will read it — the draft, not the saved file, so the preview
            tracks an edit you have not committed yet. */}
        <TabsContent value="preview">
          <div className="max-h-[32rem] overflow-auto rounded-lg border border-border bg-muted/20 p-4">
            <Markdown text={preview} />
          </div>
        </TabsContent>
      </Tabs>

      {a && (
        <div className="break-all font-mono text-[11px] text-muted-foreground/70" title={a.path}>
          {a.path}
        </div>
      )}
    </div>
  );
}
