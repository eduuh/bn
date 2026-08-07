import { useQuery } from "@tanstack/react-query";
import { ShieldQuestion } from "lucide-react";
import {
  answerPrompt,
  fleetPromptsQuery,
  type FleetPrompt,
  type PromptSemantic,
} from "../api/prompt";
import { useAppMutation } from "../hooks/useAppMutation";
import { affects } from "../lib/invalidation";
import { pushToast } from "../lib/toast";
import { Button } from "./ui/button";

/**
 * Every agent currently blocked on a dialog, with what it is asking and a one-click answer.
 *
 * The fleet board could always say an agent "needs you". It could not say *what for*, so finding
 * out meant opening each one in turn. This is the "who is waiting, and on what" strip: one batched
 * read for the whole fleet, the harness's own question on each row, and the two answers worth
 * offering at this density.
 *
 * Only the unambiguous semantics get a button here — approve, allow-and-stop-asking, and a plain
 * decline. A "No, and tell the agent what to do differently" opens a composer the agent then waits
 * on, which is not something to do in passing; those go through the full panel where you can see
 * what you are choosing.
 */
export function PendingApprovals() {
  const { data: prompts = [] } = useQuery(fleetPromptsQuery());

  // Hand-written toast for the same reason as the full panel: `ok: false` with `prompt_moved` is a
  // resolved request that answered nothing, so the tone comes from the payload rather than from
  // whether the POST succeeded.
  const answer = useAppMutation<
    Awaited<ReturnType<typeof answerPrompt>>,
    { target: string; semantic: PromptSemantic; fingerprint: number }
  >({
    mutationFn: (v) => answerPrompt(v.target, { semantic: v.semantic }, v.fingerprint),
    invalidates: affects.approval,
    toast: { error: (e) => e.message },
    onSuccess: (res, v) => {
      if (res.ok) pushToast(`${v.target}: ${res.label ?? "answered"}`, "success");
      else if (res.code === "prompt_moved")
        pushToast(`${v.target} moved on — nothing was sent`, "error");
      else pushToast(res.message ?? "could not answer", "error");
    },
  });

  if (prompts.length === 0) return null;

  return (
    <section
      aria-label="agents waiting on you"
      className="border-b border-amber-500/30 bg-amber-500/5 px-3.5 py-3"
    >
      <h2 className="mb-2 flex items-center gap-1.5 text-[11px] font-semibold uppercase tracking-wide text-amber-700 dark:text-amber-400">
        <ShieldQuestion className="h-3.5 w-3.5" />
        Waiting on you · {prompts.length}
      </h2>
      <ul className="space-y-2.5">
        {prompts.map((p: FleetPrompt) => {
          const has = (s: PromptSemantic) => p.prompt.options.some((o) => o.semantic === s);
          const act = (semantic: PromptSemantic) =>
            answer.mutate({ target: p.target, semantic, fingerprint: p.prompt.fingerprint });
          return (
            <li key={p.target} className="space-y-1.5">
              <p className="truncate font-mono text-xs text-foreground">{p.target}</p>
              <p className="text-xs text-muted-foreground">{p.prompt.question}</p>
              {p.prompt.subject && (
                <p className="truncate font-mono text-[11px] text-muted-foreground/70">
                  {p.prompt.subject}
                </p>
              )}
              <div className="flex flex-wrap gap-1.5">
                {has("approve") && (
                  <Button size="sm" disabled={answer.isPending} onClick={() => act("approve")}>
                    Approve
                  </Button>
                )}
                {has("approve-session") && (
                  <Button
                    size="sm"
                    variant="secondary"
                    disabled={answer.isPending}
                    onClick={() => act("approve-session")}
                  >
                    Approve · stop asking
                  </Button>
                )}
                {has("reject") && (
                  <Button
                    size="sm"
                    variant="destructive"
                    disabled={answer.isPending}
                    onClick={() => act("reject")}
                  >
                    Decline
                  </Button>
                )}
              </div>
            </li>
          );
        })}
      </ul>
    </section>
  );
}
