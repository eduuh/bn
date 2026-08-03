import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, BookOpen, Loader2, Plus, Save, Trash2, X } from "lucide-react";
import { queryKeys, skillDetailQuery, skillsQuery } from "../api/client";
import type { SkillInfo, SkillMutation, SkillNativeSync, SkillScope } from "../api/types";
import { SKILL_SCOPES } from "../api/types";
import { createSkill, deleteSkill, saveSkill } from "../api/writes";
import { pushToast } from "../lib/toast";
import { ToastStack } from "../components/ToastStack";
import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
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
import { cn } from "../lib/utils";
import { PageHead } from "../components/PageHead";

/** One-line summary of the Claude/Copilot native-skill sync that rides on every mutation, so a
 * stale harness is visible rather than silent. Each root syncs independently, so a partial result
 * names both halves: what was refreshed and what refused. */
function nativeSummary(native: SkillNativeSync): string {
  const { created, updated, removed, harnesses, failures } = native;
  const where = harnesses.length ? harnesses.join(" + ") : "no harness";
  const ok =
    created + updated + removed === 0 ? `${where} already current` : `synced to ${where}`;
  if (!failures?.length) return ok;
  const bad = failures.map((f) => `${f.harness} (${f.scope}): ${f.error}`).join("; ");
  return harnesses.length ? `${ok} — but ${bad}` : bad;
}

/** Toast the outcome of a skill mutation, folding the harness sync into the same line: a source
 * write that lands but fails to reach Claude/Copilot is a warning, not a success. */
function reportMutation(message: string, result: SkillMutation) {
  const detail = nativeSummary(result.native);
  pushToast(`${message} — ${detail}`, result.native.synced ? "success" : "error");
}

/** The create form (#483): name + description + tier, with an optional starter body. The server
 * writes `<tier>/<name>.md` and materializes it into the native Claude/Copilot skill roots, so a
 * skill created here is invocable in both harnesses without a CLI round trip. */
function NewSkillForm({ onDone, onCancel }: { onDone: (name: string) => void; onCancel: () => void }) {
  const qc = useQueryClient();
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [scope, setScope] = useState<SkillScope>("global-personal");
  const [body, setBody] = useState("");

  const create = useMutation({
    mutationFn: () => createSkill({ name: name.trim(), description: description.trim(), scope, body }),
    onSuccess: (result) => {
      reportMutation(`Created skill “${name.trim()}”`, result);
      void qc.invalidateQueries({ queryKey: queryKeys.skills });
      onDone(name.trim());
    },
    onError: (e) => pushToast(`Create failed: ${(e as Error).message}`, "error"),
  });

  // The server applies the same rules; mirroring them here keeps the button honest.
  const nameOk = /^[A-Za-z0-9_-]{1,64}$/.test(name.trim());
  const ready = nameOk && description.trim().length > 0;

  return (
    <Card className="space-y-3 p-4">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold text-foreground">New skill</h3>
        <Button variant="ghost" size="icon" aria-label="Cancel" onClick={onCancel}>
          <X className="h-4 w-4" />
        </Button>
      </div>

      <div className="grid gap-3 sm:grid-cols-2">
        <div className="space-y-1.5">
          <Label htmlFor="skill-name">Name</Label>
          <Input
            id="skill-name"
            value={name}
            spellCheck={false}
            placeholder="ship-it"
            onChange={(e) => setName(e.target.value)}
            className="font-mono"
          />
          {name.trim() !== "" && !nameOk && (
            <p className="text-xs text-urgent">letters, digits, “-” and “_” only (max 64)</p>
          )}
        </div>
        <div className="space-y-1.5">
          <Label htmlFor="skill-scope">Tier</Label>
          <Select value={scope} onValueChange={(v) => setScope(v as SkillScope)}>
            <SelectTrigger id="skill-scope">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {SKILL_SCOPES.map((s) => (
                <SelectItem key={s} value={s}>
                  {s}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      </div>

      <div className="space-y-1.5">
        <Label htmlFor="skill-description">Description</Label>
        <Input
          id="skill-description"
          value={description}
          placeholder="What this workflow does, in one line"
          onChange={(e) => setDescription(e.target.value)}
        />
      </div>

      <div className="space-y-1.5">
        <Label htmlFor="skill-body">Body (optional)</Label>
        <Textarea
          id="skill-body"
          className="min-h-[10rem] font-mono text-[13px] leading-relaxed"
          value={body}
          spellCheck={false}
          placeholder="The markdown instructions this skill renders as a prompt."
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
              <Plus className="h-4 w-4" /> Create skill
            </>
          )}
        </Button>
        <span className="text-xs text-muted-foreground">
          also installed for Claude and Copilot
        </span>
      </div>
    </Card>
  );
}

/** The editor pane for the selected skill: loads its raw markdown, edits it in a textarea, and
 * saves it back via `PUT /api/skills/:name` (or deletes it via `DELETE`). Only the *effective* file
 * is touched (what bn-mcp serves); the content is validated server-side, so a bad edit is rejected,
 * never written. Every write re-syncs the native Claude/Copilot skills. */
function SkillEditor({ name, onDeleted }: { name: string; onDeleted: () => void }) {
  const qc = useQueryClient();
  const detail = useQuery(skillDetailQuery(name));
  const [draft, setDraft] = useState("");
  const [dirty, setDirty] = useState(false);
  const [confirmingDelete, setConfirmingDelete] = useState(false);

  // Load the fetched content into the editable draft whenever a fresh skill loads.
  useEffect(() => {
    if (detail.data) {
      setDraft(detail.data.content);
      setDirty(false);
    }
  }, [detail.data]);

  // A different skill selected: drop any half-armed delete so it can't fire on the wrong one.
  useEffect(() => setConfirmingDelete(false), [name]);

  const save = useMutation({
    mutationFn: () => saveSkill(name, draft),
    onSuccess: (result) => {
      reportMutation(`Saved skill “${name}”`, result);
      setDirty(false);
      void qc.invalidateQueries({ queryKey: queryKeys.skill(name) });
      void qc.invalidateQueries({ queryKey: queryKeys.skills });
    },
    onError: (e) => {
      pushToast(`Save failed: ${(e as Error).message}`, "error");
    },
  });

  const remove = useMutation({
    mutationFn: () => deleteSkill(name),
    onSuccess: (result) => {
      // Deleting the effective definition unshadows a lower tier when one exists — say which,
      // rather than implying the name is gone.
      const fate = result.remaining_scope
        ? `now served from ${result.remaining_scope}`
        : "removed from every tier";
      reportMutation(`Deleted skill “${name}” (${fate})`, result);
      void qc.invalidateQueries({ queryKey: queryKeys.skill(name) });
      void qc.invalidateQueries({ queryKey: queryKeys.skills });
      onDeleted();
    },
    onError: (e) => {
      pushToast(`Delete failed: ${(e as Error).message}`, "error");
      setConfirmingDelete(false);
    },
  });

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

  const info = detail.data?.skill;
  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-2">
        <h3 className="font-mono text-sm font-semibold text-foreground">{name}</h3>
        {info && <Badge variant="muted">{info.scope}</Badge>}
      </div>
      {info?.description && <p className="text-sm text-muted-foreground">{info.description}</p>}
      <Textarea
        aria-label={`Edit ${name} skill`}
        className="min-h-[24rem] font-mono text-[13px] leading-relaxed"
        value={draft}
        spellCheck={false}
        onChange={(e) => {
          setDraft(e.target.value);
          setDirty(true);
        }}
        rows={22}
      />
      <div className="flex flex-wrap items-center gap-3">
        <Button disabled={save.isPending || !dirty} onClick={() => save.mutate()}>
          {save.isPending ? (
            <>
              <Loader2 className="h-4 w-4 animate-spin" /> Saving…
            </>
          ) : (
            <>
              <Save className="h-4 w-4" /> Save skill
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
      {info && <p className="font-mono text-xs text-muted-foreground/80">{info.path}</p>}
    </div>
  );
}

/** One row in the skill list — its effective/tier badges and a one-line description. */
function SkillRow({
  skill,
  selected,
  onSelect,
}: {
  skill: SkillInfo;
  selected: boolean;
  onSelect: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onSelect}
      className={cn(
        "flex w-full items-center gap-2 rounded-lg border px-3 py-2 text-left transition-colors",
        selected
          ? "border-primary/40 bg-primary/10"
          : "border-border bg-card hover:border-border/80 hover:bg-accent/40",
        !skill.effective && "opacity-70",
      )}
    >
      <span className="min-w-0 flex-1 truncate font-mono text-sm text-foreground">{skill.name}</span>
      <span className="hidden shrink-0 text-xs text-muted-foreground sm:inline">{skill.scope}</span>
      {skill.effective ? (
        <Badge variant="ok" title="the definition bn-mcp serves">
          effective
        </Badge>
      ) : (
        <Badge variant="muted" title="shadowed by a higher tier">
          shadowed
        </Badge>
      )}
    </button>
  );
}

/**
 * `/skills` — the skill library (#483). Every skill across the personal / global / per-repo tiers
 * (what bn-mcp serves as prompts), fully editable from the dashboard: create one in any tier, edit
 * its markdown, or delete it. Writes are validated server-side so a bad edit can't corrupt a skill,
 * and each one re-materializes the native Claude (`~/.claude/skills`) and Copilot
 * (`~/.copilot/skills`) skills — so what the dashboard shows is what both harnesses run.
 */
export function Skills() {
  const skills = useQuery(skillsQuery());
  const [selected, setSelected] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);

  const all = skills.data?.skills ?? [];
  // Effective definitions first, then shadowed; alpha within each.
  const sorted = [...all].sort(
    (a, b) => Number(b.effective) - Number(a.effective) || a.name.localeCompare(b.name),
  );

  return (
    <div className="space-y-5 pb-8">
      <PageHead
        title="Skills"
        sub={
          skills.isLoading
            ? "loading…"
            : `${all.length} across all tiers · create, edit & delete · synced to Claude + Copilot`
        }
        right={
          !creating && (
            <Button onClick={() => setCreating(true)}>
              <Plus className="h-4 w-4" /> New skill
            </Button>
          )
        }
      />

      {creating && (
        <NewSkillForm
          onCancel={() => setCreating(false)}
          onDone={(name) => {
            setCreating(false);
            setSelected(name);
          }}
        />
      )}

      {skills.isLoading && (
        <div className="space-y-2" aria-hidden="true">
          {[0, 1, 2, 3].map((i) => (
            <Skeleton key={i} className="h-10 w-full" />
          ))}
        </div>
      )}

      {skills.isError && (
        <Card className="flex items-center gap-2 border-urgent/30 bg-urgent-muted p-4 text-sm text-urgent">
          <AlertTriangle className="h-4 w-4" />
          Could not load skills.
        </Card>
      )}

      {!skills.isLoading && !skills.isError && all.length === 0 && !creating && (
        <Card className="flex flex-col items-center gap-2 px-6 py-12 text-center">
          <BookOpen className="h-8 w-8 text-muted-foreground" />
          <h3 className="text-lg font-semibold">No skills</h3>
          <p className="max-w-sm text-sm text-muted-foreground">
            No skills defined in any tier. Create one here, or with{" "}
            <code className="font-mono">bn skills new</code>.
          </p>
          <Button className="mt-2" onClick={() => setCreating(true)}>
            <Plus className="h-4 w-4" /> New skill
          </Button>
        </Card>
      )}

      {all.length > 0 && (
        <div className="grid grid-cols-1 gap-4 md:grid-cols-[minmax(0,18rem)_1fr]">
          <div className="space-y-2">
            {sorted.map((s) => (
              <SkillRow
                key={`${s.scope}:${s.name}`}
                skill={s}
                selected={selected === s.name}
                onSelect={() => setSelected(s.name)}
              />
            ))}
          </div>
          <Card className="p-4">
            {selected ? (
              <SkillEditor name={selected} onDeleted={() => setSelected(null)} />
            ) : (
              <div className="flex flex-col items-center gap-2 px-6 py-12 text-center">
                <BookOpen className="h-8 w-8 text-muted-foreground" />
                <p className="max-w-sm text-sm text-muted-foreground">
                  Pick a skill to view and edit its source.
                </p>
              </div>
            )}
          </Card>
        </div>
      )}

      <ToastStack />
    </div>
  );
}
