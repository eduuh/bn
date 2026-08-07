import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { RotateCw, Send, Skull } from "lucide-react";
import { promptQuery } from "../api/prompt";
import type { AgentControl } from "../api/types";
import { killAgent, relaunchAgent, sendMessage } from "../api/writes";
import { useAppMutation } from "../hooks/useAppMutation";
import { affects } from "../lib/invalidation";
import { Button } from "./ui/button";
import { Card } from "./ui/card";
import { Textarea } from "./ui/textarea";

/** The steer controls (design §4(c)): a message composer → `POST send`, and relaunch (soft/hard) /
 * kill buttons → the write-layer endpoints. Results surface as toasts, and every write invalidates
 * the fleet + pane so the effect shows without a reload. */
export function SteerPanel({ target, control }: { target: string; control?: AgentControl }) {
  const [message, setMessage] = useState("");

  // bn drives agents through a recorded session (`--resume`) or a live tmux window (send-keys), and
  // a session it never launched still resumes against its own transcript — so the fleet no longer
  // carries agents it can only watch. What remains unsteerable is a genuine absence: no live
  // checkout to run a turn in, or two sessions a write could land on with no way to tell them
  // apart. `undefined` means the server predates the field, so we allow the attempt rather than
  // locking the panel against a stale binary.
  const steerable = control !== "none" && control !== "ambiguous";

  // An agent parked on a numbered menu cannot be steered with prose: the message would be typed
  // into the prompt rather than read, and a stray character can register as a menu selection
  // nobody chose. `bn agent send` refuses this case for the same reason, so the two surfaces
  // agree — the answer is a button in the approval panel, not a sentence here.
  const { data: pending } = useQuery(promptQuery(target));
  const blocked = !!pending;

  // No optimistic patch on these three: what changes is the agent's own progress, which only the
  // server can report. The honest feedback is the pending button plus the toast, and the reconcile
  // on settle then pulls the real state — including when the write failed.
  const sendMut = useAppMutation({
    mutationFn: () => sendMessage(target, message.trim()),
    invalidates: affects.agentSteer,
    toast: { success: "message sent", error: "send failed" },
    onSuccess: () => setMessage(""),
  });
  const relaunchMut = useAppMutation<unknown, boolean>({
    mutationFn: (hard) => relaunchAgent(target, hard),
    invalidates: affects.agentSteer,
    toast: {
      success: (_d, hard) => (hard ? "hard relaunch triggered" : "relaunch triggered"),
      error: "relaunch failed",
    },
  });
  const killMut = useAppMutation({
    mutationFn: () => killAgent(target),
    invalidates: affects.agentSteer,
    toast: { success: "agent killed", error: "kill failed" },
  });

  const busy = sendMut.isPending || relaunchMut.isPending || killMut.isPending || !steerable;
  const canCompose = steerable && !blocked;

  return (
    <Card className="space-y-3 p-4">
      <h3 className="text-sm font-semibold uppercase tracking-wide text-muted-foreground">Steer</h3>

      {!steerable && (
        <p
          role="status"
          className="rounded-md border border-border bg-muted px-2.5 py-2 text-xs text-muted-foreground"
        >
          {control === "ambiguous" ? (
            <>
              <span className="font-medium text-foreground">
                More than one session shares this branch.
              </span>{" "}
              Every write channel here is keyed by <code className="font-mono">{target}</code>, and
              neither session has a live checkout to resume into, so bn cannot tell which one a
              message — or a kill — would reach. Steering is disabled until only one is left.
            </>
          ) : (
            <>
              <span className="font-medium text-foreground">No live checkout.</span> bn steers a
              session by running a <code className="font-mono">--resume</code> turn in the directory
              it is working in, and this one's checkout is gone or was never recorded — so there is
              nowhere to run the turn. Drive it from the terminal it is running in.
            </>
          )}
        </p>
      )}

      {blocked && (
        <p
          role="status"
          className="rounded-md border border-amber-500/40 bg-amber-500/5 px-2.5 py-2 text-xs text-muted-foreground"
        >
          <span className="font-medium text-foreground">This agent is waiting on a dialog.</span> A
          message typed here would land in the prompt instead of being read. Answer it with the
          buttons above, then steer.
        </p>
      )}

      <form
        className="space-y-3"
        onSubmit={(e) => {
          e.preventDefault();
          if (message.trim()) sendMut.mutate();
        }}
      >
        <Textarea
          className="min-h-[5rem] font-mono text-[13px] leading-relaxed"
          value={message}
          onChange={(e) => setMessage(e.target.value)}
          disabled={!canCompose}
          placeholder={
            blocked
              ? "Answer the dialog above first"
              : steerable
                ? "Message the agent (⌘/Ctrl+Enter to send)…"
                : "Not steerable from here — no window or recorded session"
          }
          rows={3}
          onKeyDown={(e) => {
            if ((e.metaKey || e.ctrlKey) && e.key === "Enter" && message.trim()) sendMut.mutate();
          }}
        />
        <div className="flex flex-wrap gap-2">
          <Button type="submit" size="sm" disabled={busy || blocked || !message.trim()}>
            <Send className="h-4 w-4" /> Send
          </Button>
          <Button
            type="button"
            variant="secondary"
            size="sm"
            disabled={busy}
            onClick={() => relaunchMut.mutate(false)}
            title="Recover an interrupted ship turn"
          >
            <RotateCw className="h-4 w-4" /> Relaunch
          </Button>
          <Button
            type="button"
            variant="secondary"
            size="sm"
            disabled={busy}
            onClick={() => {
              if (window.confirm("Hard relaunch this agent?")) relaunchMut.mutate(true);
            }}
          >
            Relaunch (hard)
          </Button>
          <Button
            type="button"
            variant="destructive"
            size="sm"
            disabled={busy}
            onClick={() => {
              if (window.confirm(`Kill ${target}? This closes its tmux window.`)) killMut.mutate();
            }}
          >
            <Skull className="h-4 w-4" /> Kill
          </Button>
        </div>
      </form>
    </Card>
  );
}
