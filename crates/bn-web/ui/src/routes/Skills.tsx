import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, BookOpen, Loader2, Save } from "lucide-react";
import { queryKeys, skillDetailQuery, skillsQuery } from "../api/client";
import type { SkillInfo } from "../api/types";
import { saveSkill } from "../api/writes";
import { pushToast } from "../lib/toast";
import { ToastStack } from "../components/ToastStack";
import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
import { Skeleton } from "../components/ui/skeleton";
import { Textarea } from "../components/ui/textarea";
import { cn } from "../lib/utils";

/** The editor pane for the selected skill: loads its raw markdown, edits it in a textarea, and
 * saves it back via `PUT /api/skills/:name`. Only the *effective* file is edited (what bn-mcp
 * serves); the content is validated server-side, so a bad edit is rejected, never written. */
function SkillEditor({ name }: { name: string }) {
  const qc = useQueryClient();
  const detail = useQuery(skillDetailQuery(name));
  const [draft, setDraft] = useState("");
  const [dirty, setDirty] = useState(false);

  // Load the fetched content into the editable draft whenever a fresh skill loads.
  useEffect(() => {
    if (detail.data) {
      setDraft(detail.data.content);
      setDirty(false);
    }
  }, [detail.data]);

  const save = useMutation({
    mutationFn: () => saveSkill(name, draft),
    onSuccess: () => {
      pushToast(`Saved skill “${name}”`, "success");
      setDirty(false);
      void qc.invalidateQueries({ queryKey: queryKeys.skill(name) });
      void qc.invalidateQueries({ queryKey: queryKeys.skills });
    },
    onError: (e) => {
      pushToast(`Save failed: ${(e as Error).message}`, "error");
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
      <div className="flex items-center gap-3">
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
 * (what bn-mcp serves as prompts), each viewable and **editable** from the dashboard: pick one, edit
 * its markdown, save it back (validated so a bad edit can't corrupt it). A paired token authorizes
 * the write. The effective (highest-precedence) definition is the one edited.
 */
export function Skills() {
  const skills = useQuery(skillsQuery());
  const [selected, setSelected] = useState<string | null>(null);

  const all = skills.data?.skills ?? [];
  // Effective definitions first, then shadowed; alpha within each.
  const sorted = [...all].sort(
    (a, b) => Number(b.effective) - Number(a.effective) || a.name.localeCompare(b.name),
  );

  return (
    <div className="mx-auto max-w-4xl space-y-5 pb-8">
      <header className="space-y-1">
        <h2 className="text-2xl font-semibold tracking-tight">Skills</h2>
        <p className="text-sm text-muted-foreground">
          {skills.isLoading ? "loading…" : `${all.length} across all tiers · view & edit`}
        </p>
      </header>

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

      {!skills.isLoading && !skills.isError && all.length === 0 && (
        <Card className="flex flex-col items-center gap-2 px-6 py-12 text-center">
          <BookOpen className="h-8 w-8 text-muted-foreground" />
          <h3 className="text-lg font-semibold">No skills</h3>
          <p className="max-w-sm text-sm text-muted-foreground">
            No skills defined in any tier. Create one with <code className="font-mono">bn skill new</code>.
          </p>
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
              <SkillEditor name={selected} />
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
