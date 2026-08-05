import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { AlertTriangle, Loader2 } from "lucide-react";
import { learningsQuery, type LearningsFilter } from "../api/client";
import type { LearningEntry } from "../api/types";
import { Badge, type BadgeProps } from "./ui/badge";
import { Button } from "./ui/button";
import { Card } from "./ui/card";
import { Input } from "./ui/input";
import { cn } from "../lib/utils";

const WINDOWS: { label: string; value: number | "all" }[] = [
  { label: "7d", value: 7 },
  { label: "30d", value: 30 },
  { label: "90d", value: 90 },
  { label: "All", value: "all" },
];

const AUTHORS: { label: string; value: string }[] = [
  { label: "Everyone", value: "" },
  { label: "Agents", value: "agent" },
  { label: "Mine", value: "mine" },
];

/** A segmented toggle group — the shared window / author picker built from buttons (mirrors the
 * composer's `Segment`, matched to the Phase-1 look). */
function SegmentGroup<T extends string | number>({
  label,
  options,
  value,
  onChange,
}: {
  label: string;
  options: { label: string; value: T }[];
  value: T;
  onChange: (v: T) => void;
}) {
  return (
    <div
      className="inline-flex gap-1 rounded-lg bg-muted p-1"
      role="group"
      aria-label={label}
    >
      {options.map((o) => (
        <button
          key={o.label}
          type="button"
          onClick={() => onChange(o.value)}
          aria-pressed={o.value === value}
          className={cn(
            "rounded-md px-3 py-1 text-sm font-medium transition-colors",
            o.value === value
              ? "bg-card text-foreground shadow-card"
              : "text-muted-foreground hover:text-foreground",
          )}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

/** The review decision chip for an entry, or the read-only Promote/Dismiss affordance when it's
 * still unreviewed. The write path (`learnings::review`, #322) isn't exposed over HTTP yet, so the
 * buttons are disabled and annotated rather than wired — the design's "else render read-only". */
function ReviewControls({ entry }: { entry: LearningEntry }) {
  const decision = entry.review?.decision;
  if (decision) {
    const variant: BadgeProps["variant"] =
      decision === "promoted" ? "ok" : decision === "dismissed" ? "muted" : "warn";
    return <Badge variant={variant}>{decision}</Badge>;
  }
  return (
    <div className="flex items-center gap-1" title="review endpoint pending (#322)">
      <Button type="button" variant="outline" size="sm" disabled>
        Promote
      </Button>
      <Button type="button" variant="outline" size="sm" disabled>
        Dismiss
      </Button>
    </div>
  );
}

/**
 * The Learnings tab (design §4(e) / #322): the reflections feed with since / repo / skill /
 * agent-vs-mine filters, grouped by theme, with headline counts and per-entry review controls.
 * Promote/Dismiss are read-only until the learnings-review endpoint lands (noted inline).
 */
export function LearningsFeed() {
  const [windowSel, setWindowSel] = useState<number | "all">(7);
  const [repo, setRepo] = useState("");
  const [skill, setSkill] = useState("");
  const [author, setAuthor] = useState("");

  const filter: LearningsFilter = {
    since: windowSel === "all" ? undefined : windowSel,
    all: windowSel === "all",
    repo: repo.trim() || undefined,
    skill: skill.trim() || undefined,
    agent: author || undefined,
  };
  const q = useQuery(learningsQuery(filter));
  const view = q.data;

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-2">
        <SegmentGroup
          label="window"
          options={WINDOWS}
          value={windowSel}
          onChange={setWindowSel}
        />
        <SegmentGroup label="author" options={AUTHORS} value={author} onChange={setAuthor} />
        <Input
          className="h-8 w-28"
          value={repo}
          onChange={(e) => setRepo(e.target.value)}
          placeholder="repo…"
        />
        <Input
          className="h-8 w-28"
          value={skill}
          onChange={(e) => setSkill(e.target.value)}
          placeholder="skill…"
        />
      </div>

      <p className="text-xs text-muted-foreground">
        Promote / Dismiss is read-only until the learnings-review endpoint (#322) is exposed.
      </p>

      {q.isLoading && (
        <div className="flex items-center gap-2 text-sm text-muted-foreground">
          <Loader2 className="h-4 w-4 animate-spin" />
          Loading learnings…
        </div>
      )}
      {q.isError && (
        <Card className="flex items-center gap-2 border-urgent/30 bg-urgent-muted p-3 text-sm text-urgent">
          <AlertTriangle className="h-4 w-4" />
          Could not load learnings.
        </Card>
      )}

      {view && (
        <>
          <div className="flex flex-wrap items-center gap-1.5">
            <Badge variant="muted">{view.total} total</Badge>
            <Badge variant="muted">{view.learned} learned</Badge>
            <Badge variant="muted">{view.reflections} reflections</Badge>
            <Badge variant="muted">{view.agent} agent</Badge>
            <Badge variant="muted">{view.mine} mine</Badge>
            {view.unreviewed > 0 && <Badge variant="warn">{view.unreviewed} unreviewed</Badge>}
            {view.promoted > 0 && <Badge variant="ok">{view.promoted} promoted</Badge>}
            {view.dismissed > 0 && <Badge variant="muted">{view.dismissed} dismissed</Badge>}
          </div>

          {view.groups.length === 0 && (
            <Card className="flex items-center justify-center p-8 text-sm text-muted-foreground">
              No learnings in this window.
            </Card>
          )}

          {view.groups.map((g) => (
            <section className="space-y-2" key={g.theme}>
              <h4 className="flex items-center gap-2 text-sm font-semibold text-foreground">
                {g.theme}
                <span className="text-xs font-normal text-muted-foreground">
                  {g.entries.length}
                </span>
              </h4>
              <div className="space-y-2">
                {g.entries.map((e) => (
                  <Card key={e.key} className="space-y-2 p-3">
                    <div className="flex items-start gap-2">
                      <Badge variant={e.kind === "learned" ? "ok" : "muted"} className="shrink-0">
                        {e.kind}
                      </Badge>
                      <span className="min-w-0 flex-1 text-sm text-foreground/90">{e.text}</span>
                    </div>
                    <div className="flex flex-wrap items-center gap-1.5">
                      <Badge variant="muted">{e.date}</Badge>
                      {e.repo && <Badge variant="muted">{e.repo}</Badge>}
                      {e.skill && <Badge variant="muted">skill:{e.skill}</Badge>}
                      {e.harness && <Badge variant="muted">{e.harness}</Badge>}
                      <Badge variant="muted">{e.agent ? "agent" : "mine"}</Badge>
                      <span className="flex-1" />
                      <ReviewControls entry={e} />
                    </div>
                  </Card>
                ))}
              </div>
            </section>
          ))}
        </>
      )}
    </div>
  );
}
