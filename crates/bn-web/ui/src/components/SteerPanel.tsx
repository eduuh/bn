import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { RotateCw, Send, Skull } from "lucide-react";
import { queryKeys } from "../api/client";
import { killAgent, relaunchAgent, sendMessage } from "../api/writes";
import { pushToast } from "../lib/toast";
import { Button } from "./ui/button";
import { Card } from "./ui/card";
import { Textarea } from "./ui/textarea";

/** The steer controls (design §4(c)): a message composer → `POST send`, and relaunch (soft/hard) /
 * kill buttons → the write-layer endpoints. Results surface as toasts, and every write invalidates
 * the fleet + pane so the effect shows without a reload. */
export function SteerPanel({ target }: { target: string }) {
  const qc = useQueryClient();
  const [message, setMessage] = useState("");

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

  const busy = sendMut.isPending || relaunchMut.isPending || killMut.isPending;

  return (
    <Card className="space-y-3 p-4">
      <h3 className="text-sm font-semibold uppercase tracking-wide text-muted-foreground">Steer</h3>

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
          placeholder="Message the agent (⌘/Ctrl+Enter to send)…"
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
