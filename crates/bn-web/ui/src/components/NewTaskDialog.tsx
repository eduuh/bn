import { useState } from "react";
import type { TaskCreate } from "../api/writes";
import { Button } from "./ui/button";
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "./ui/dialog";

/**
 * The only way to originate work by hand (#1006).
 *
 * It replaces **Start an agent**, and the difference is the point: that spawned a process from a
 * free-text prompt and left nothing behind, so the intent vanished with the agent. This writes a
 * durable record first, and an agent picks it up afterwards.
 *
 * Only the title is required. Everything else is what the SMART gate (#998) will ask for anyway —
 * offering the fields here saves a round trip through the improve agent, but a bare title is a
 * legitimate capture: better a rough task written down than a good one lost while you look up the
 * estimate.
 */
export function NewTaskDialog({
  open,
  busy,
  onOpenChange,
  onSubmit,
}: {
  open: boolean;
  busy?: boolean;
  onOpenChange: (open: boolean) => void;
  onSubmit: (body: TaskCreate) => void;
}) {
  const [title, setTitle] = useState("");
  const [repo, setRepo] = useState("");
  const [day, setDay] = useState("");
  const [est, setEst] = useState("");

  const submit = () => {
    if (!title.trim()) return;
    onSubmit({
      title: title.trim(),
      repo: repo.trim() || undefined,
      day: day.trim() || undefined,
      est: est.trim() || undefined,
    });
    setTitle("");
    setRepo("");
    setDay("");
    setEst("");
  };

  const field =
    "w-full rounded-[7px] border border-border bg-background px-2.5 py-1.5 text-sm text-foreground placeholder:text-muted-foreground/70 focus:border-primary focus:outline-none";

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>New task</DialogTitle>
        </DialogHeader>
        <div className="space-y-2.5">
          <label className="block space-y-1">
            <span className="text-[11px] font-medium text-muted-foreground">
              What changes, and where
            </span>
            <input
              autoFocus
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) submit();
              }}
              placeholder="Fix the SSE reconnect storm on flaky LAN"
              className={field}
            />
          </label>
          <div className="grid grid-cols-3 gap-2">
            <label className="block space-y-1">
              <span className="text-[11px] font-medium text-muted-foreground">Repo</span>
              <input
                value={repo}
                onChange={(e) => setRepo(e.target.value)}
                placeholder="bn"
                className={field}
              />
            </label>
            <label className="block space-y-1">
              <span className="text-[11px] font-medium text-muted-foreground">Day</span>
              <input
                value={day}
                onChange={(e) => setDay(e.target.value)}
                placeholder="2026-08-03"
                className={field}
              />
            </label>
            <label className="block space-y-1">
              <span className="text-[11px] font-medium text-muted-foreground">Estimate</span>
              <input
                value={est}
                onChange={(e) => setEst(e.target.value)}
                placeholder="2h"
                className={field}
              />
            </label>
          </div>
          <p className="text-[11px] leading-snug text-muted-foreground">
            Only the title is required. A task needs a repo, an estimate, a date and a{" "}
            <code className="font-mono">## Done When</code> before an agent may execute it — the
            improve agent fills in whatever you leave out.
          </p>
          <div className="flex justify-end gap-2 pt-1">
            <Button variant="secondary" size="sm" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button size="sm" disabled={busy || !title.trim()} onClick={submit}>
              Create
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
