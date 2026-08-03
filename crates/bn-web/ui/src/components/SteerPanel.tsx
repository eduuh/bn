import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { RotateCw, Send, Skull } from "lucide-react";
import { queryKeys } from "../api/client";
import type { AgentControl } from "../api/types";
import { killAgent, relaunchAgent, sendMessage } from "../api/writes";
import { pushToast } from "../lib/toast";
import { Button } from "./ui/button";
import { Card } from "./ui/card";
import { Textarea } from "./ui/textarea";

/** The steer controls (design §4(c)): a message composer → `POST send`, and relaunch (soft/hard) /
 * kill buttons → the write-layer endpoints. Results surface as toasts, and every write invalidates
 * the fleet + pane so the effect shows without a reload. */
export function SteerPanel({ target, control }: { target: string; control?: AgentControl }) {
  const qc = useQueryClient();
  const [message, setMessage] = useState("");

  // bn drives agents through a recorded session (`--resume`) or a live tmux window (send-keys). The
  // fleet *observes* every running session on the machine, including ones bn never launched — for
  // those there is no write channel at all, and every button here fails after the click with
  // "no such tmux window, or tmux absent". Say so up front instead. `undefined` means the server
  // predates the field, so we allow the attempt rather than locking the panel against a stale
  // binary.
  const steerable = control !== "none" && control !== "ambiguous";

  const invalidate = () => {
    void qc.invalidateQueries({ queryKey: queryKeys.fleet });
    void qc.invalidateQueries({ queryKey: queryKeys.activityAll });
  };
  const onError = (e: unknown, label: string) => {
    pushToast(`${label} failed: ${(e as Error).message}`, "error");
  };

  const sendMut = useMutation({
    mutationFn: () => sendMessage(target, message.trim()),
    onSuccess: () => {
      pushToast("message sent", "success");
      setMessage("");
      invalidate();
    },
    onError: (e) => onError(e, "send"),
  });
  const relaunchMut = useMutation({
    mutationFn: (hard: boolean) => relaunchAgent(target, hard),
    onSuccess: (_data, hard) => {
      pushToast(hard ? "hard relaunch triggered" : "relaunch triggered", "success");
      invalidate();
    },
    onError: (e) => onError(e, "relaunch"),
  });
  const killMut = useMutation({
    mutationFn: () => killAgent(target),
    onSuccess: () => {
      pushToast("agent killed", "success");
      invalidate();
    },
    onError: (e) => onError(e, "kill"),
  });

  const busy = sendMut.isPending || relaunchMut.isPending || killMut.isPending || !steerable;

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
              Every write channel is keyed by <code className="font-mono">{target}</code>, so bn
              cannot tell which session a message — or a kill — would reach. Steering is disabled
              until only one is left.
            </>
          ) : (
            <>
              <span className="font-medium text-foreground">Observed only.</span> This session was
              started outside bn, so there is no window or recorded session to write to — bn can read
              its transcript but cannot steer, relaunch or kill it. Drive it from the terminal it is
              running in.
            </>
          )}
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
          disabled={!steerable}
          placeholder={
            steerable
              ? "Message the agent (⌘/Ctrl+Enter to send)…"
              : "Not steerable from here — no window or recorded session"
          }
          rows={3}
          onKeyDown={(e) => {
            if ((e.metaKey || e.ctrlKey) && e.key === "Enter" && message.trim()) sendMut.mutate();
          }}
        />
        <div className="flex flex-wrap gap-2">
          <Button type="submit" size="sm" disabled={busy || !message.trim()}>
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
