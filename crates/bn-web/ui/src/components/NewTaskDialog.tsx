import { useState } from "react";

import type { TaskCreate } from "../api/writes";
import { Button } from "./ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "./ui/dialog";
import { Input } from "./ui/input";
import { Label } from "./ui/label";
import { RepoSelect } from "./RepoSelect";

/**
 * The only way to originate work by hand (#1006).
 *
 * It replaces **Start an agent**, and the difference is the point: that spawned a process from a
 * free-text prompt and left nothing behind, so the intent vanished with the agent. This writes a
 * durable record first, and an agent picks it up afterwards.
 *
 * Only the title is required. Everything else is what the SMART gate (#998) will ask for anyway —
 * offering the fields here saves a round trip through the enhance agent, but a bare title is a
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

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>New task</DialogTitle>
          <DialogDescription>
            Only the title is required. A task needs a repo, an estimate, a date and a{" "}
            <code className="font-mono">## Done When</code> before an agent may execute it — the
            enhance agent fills in whatever you leave out.
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-3">
          <div className="space-y-1.5">
            <Label htmlFor="task-title" className="text-[11px]">
              What changes, and where
            </Label>
            <Input
              id="task-title"
              autoFocus
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              // ⌘/Ctrl+Enter submits from the title field so a bare capture never needs the mouse.
              onKeyDown={(e) => {
                if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) submit();
              }}
              placeholder="Fix the SSE reconnect storm on flaky LAN"
            />
          </div>

          <div className="grid grid-cols-3 gap-2">
            <div className="space-y-1.5">
              <Label className="text-[11px]">Repo</Label>
              <RepoSelect value={repo} onChange={setRepo} />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="task-day" className="text-[11px]">
                Day
              </Label>
              <Input
                id="task-day"
                value={day}
                onChange={(e) => setDay(e.target.value)}
                placeholder="2026-08-03"
              />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="task-est" className="text-[11px]">
                Estimate
              </Label>
              <Input
                id="task-est"
                value={est}
                onChange={(e) => setEst(e.target.value)}
                placeholder="2h"
              />
            </div>
          </div>
        </div>

        <DialogFooter>
          <Button variant="secondary" size="sm" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button size="sm" disabled={busy || !title.trim()} onClick={submit}>
            Create
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
