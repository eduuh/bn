import { useState } from "react";
import { Plus } from "lucide-react";
import type { ActionItem } from "../../api/types";
import { StartAgentComposer } from "../StartAgentComposer";
import { Button } from "../ui/button";

/** A blank action standing in for "I just want to start something." The composer is written
 * against an [`ActionItem`], so free-form start is simply an item with an empty prompt — no second
 * code path, and the same dispatch/queue write behind it. */
function blankAction(repo: string): ActionItem {
  return {
    id: "adhoc",
    type: "todo",
    title: "New work",
    source: "you",
    severity: "info",
    repo: repo || null,
    branch: null,
    link: null,
    detail: null,
    payload: repo ? { repo } : {},
    suggested_prompt: "",
    default_urgency: "urgent",
    created: 0,
    stale: false,
  };
}

/**
 * The console's single, always-present way to begin work. It sits in the queue header on every
 * view, so starting an agent is never a matter of finding the right screen first — which is what
 * the separate `/start` route and the per-row "Start agent" buttons each half-solved.
 *
 * `repo` pre-fills from whatever is selected, so the common case (start something else in the repo
 * I'm already looking at) takes one click and a sentence.
 */
export function StartButton({ repo = "" }: { repo?: string }) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <Button
        size="sm"
        className="h-8 w-full"
        onClick={() => setOpen(true)}
        title="start an agent on anything"
      >
        <Plus className="h-4 w-4" /> Start
      </Button>
      {open && <StartAgentComposer item={blankAction(repo)} onClose={() => setOpen(false)} />}
    </>
  );
}
