import { useQuery } from "@tanstack/react-query";
import { ShieldQuestion } from "lucide-react";
import {
  answerPrompt,
  promptQuery,
  semanticLabel,
  semanticTone,
  type PromptOption,
} from "../api/prompt";
import { useAppMutation } from "../hooks/useAppMutation";
import { affects } from "../lib/invalidation";
import { pushToast } from "../lib/toast";
import { Button } from "./ui/button";
import { Card } from "./ui/card";

/**
 * The dialog an agent is blocked on, with one button per option the harness actually offered.
 *
 * The buttons carry the harness's own wording and its own numbering. That is the whole point: the
 * dashboard used to have no way to answer a permission prompt at all, and the tmux quick-steer that
 * did offered a generic "Approve (1) / Approve (2)" that fired a bare digit — on Claude's dialog,
 * option 3 is "No, and tell Claude what to do differently", so a guess could turn an approval into
 * a rejection into a composer nobody was going to fill in.
 *
 * Every click sends the `fingerprint` the dialog was read at. If the pane moved in between — the
 * agent answered it itself, or moved on to a *different* prompt — the server refuses and we re-read
 * rather than retrying, so a tab left open on a stale dialog can never approve the wrong thing.
 *
 * Renders nothing when the agent is not blocked, so it can sit unconditionally in the page.
 */
export function ApprovalPanel({ target }: { target: string }) {
  const { data: prompt } = useQuery(promptQuery(target));

  // The toast stays hand-written rather than becoming a `toast.success`: a 2xx here is not the same
  // as an answer landing. The server reports `ok: false` for a moved prompt, and an `ok` that did
  // not *verify* means the keystroke went out but the dialog is still up — three outcomes with two
  // tones between them, which only the payload can decide.
  const answer = useAppMutation<Awaited<ReturnType<typeof answerPrompt>>, { option: number }>({
    mutationFn: (choice) => {
      if (!prompt) throw new Error("no dialog on screen");
      return answerPrompt(target, choice, prompt.fingerprint);
    },
    invalidates: affects.approval,
    toast: { error: (e) => e.message },
    onSuccess: (res) => {
      if (res.ok) {
        pushToast(
          res.verified
            ? `answered: ${res.label ?? res.option}`
            : `sent ${res.option}, but the dialog is still up — re-reading`,
          res.verified ? "success" : "error",
        );
      } else if (res.code === "prompt_moved") {
        pushToast("the agent moved on — showing what it is asking now", "error");
      } else {
        pushToast(res.message ?? "could not answer", "error");
      }
    },
  });

  if (!prompt) return null;

  const kindLabel =
    prompt.kind === "trust-menu"
      ? "Waiting: trust prompt"
      : prompt.kind === "command-approval"
        ? "Waiting: permission"
        : "Waiting: decision";

  return (
    <Card className="space-y-3 border-amber-500/40 bg-amber-500/5 p-4">
      <div className="flex items-center gap-2">
        <ShieldQuestion className="h-4 w-4 text-amber-600 dark:text-amber-400" />
        <h3 className="text-sm font-semibold uppercase tracking-wide text-amber-700 dark:text-amber-400">
          {kindLabel}
        </h3>
      </div>

      {prompt.subject && (
        <p className="overflow-x-auto whitespace-pre-wrap break-words rounded-md border border-border bg-muted px-2.5 py-2 font-mono text-xs">
          {prompt.subject}
        </p>
      )}
      <p className="text-sm font-medium">{prompt.question}</p>

      <div className="flex flex-col gap-2">
        {prompt.options.map((o: PromptOption) => (
          <Button
            key={o.number}
            size="sm"
            variant={semanticTone(o.semantic)}
            className="h-auto justify-start whitespace-normal py-2 text-left"
            disabled={answer.isPending}
            onClick={() => answer.mutate({ option: o.number })}
          >
            <span className="mr-2 font-mono opacity-70">{o.number}</span>
            <span className="flex-1">
              {o.label}
              {semanticLabel(o.semantic) && (
                <span className="ml-2 text-xs opacity-70">— {semanticLabel(o.semantic)}</span>
              )}
            </span>
          </Button>
        ))}
      </div>

      {prompt.options.length === 0 && (
        <p className="text-xs text-muted-foreground">
          This prompt is blocking but lists no numbered options — open the pane to answer it.
        </p>
      )}
    </Card>
  );
}
