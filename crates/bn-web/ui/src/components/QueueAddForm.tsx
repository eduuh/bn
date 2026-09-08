import { useState } from "react";
import { Plus } from "lucide-react";

import { queueAdd } from "../api/writes";
import { useAppMutation } from "../hooks/useAppMutation";
import { affects } from "../lib/invalidation";
import { Button } from "./ui/button";
import { Input } from "./ui/input";

/** The kinds `bn queue add --kind` accepts. Mirrors `BacklogKind::ALL`. */
const KINDS = [
  "ship",
  "scout",
  "investigate",
  "research",
  "enhance",
  "review-pr",
  "cron",
  "cleanup",
];

/** Admit one piece of planned work to the durable queue. */
export function QueueAddForm() {
  const [item, setItem] = useState("");
  const [repo, setRepo] = useState("");
  const [kind, setKind] = useState("ship");
  const [priority, setPriority] = useState("0");

  const add = useAppMutation({
    mutationFn: () =>
      queueAdd({
        item,
        repo: repo.trim() || undefined,
        kind,
        priority: Number(priority) || 0,
      }),
    invalidates: affects.queue,
    toast: { success: `Queued ${item}`, error: "Could not queue" },
    onSuccess: () => setItem(""),
  });

  return (
    <form
      className="flex flex-wrap items-end gap-2"
      onSubmit={(event) => {
        event.preventDefault();
        if (item.trim()) add.mutate();
      }}
    >
      <label className="flex min-w-56 flex-1 flex-col gap-1">
        <span className="text-xs text-muted-foreground">Work</span>
        <Input
          value={item}
          onChange={(event) => setItem(event.target.value)}
          placeholder="issue number, URL, or a clear outcome"
          className="h-8"
        />
      </label>
      <label className="flex w-36 flex-col gap-1">
        <span className="text-xs text-muted-foreground">Repo</span>
        <Input
          value={repo}
          onChange={(event) => setRepo(event.target.value)}
          placeholder="current"
          className="h-8"
        />
      </label>
      <label className="flex w-36 flex-col gap-1">
        <span className="text-xs text-muted-foreground">Kind</span>
        <select
          value={kind}
          onChange={(event) => setKind(event.target.value)}
          className="h-8 rounded-md border border-input bg-background px-2 text-sm"
        >
          {KINDS.map((value) => (
            <option key={value} value={value}>
              {value}
            </option>
          ))}
        </select>
      </label>
      <label className="flex w-24 flex-col gap-1">
        <span className="text-xs text-muted-foreground">Priority</span>
        <Input
          value={priority}
          onChange={(event) => setPriority(event.target.value)}
          inputMode="numeric"
          className="h-8 tabular-nums"
        />
      </label>
      <Button type="submit" size="sm" disabled={!item.trim() || add.isPending}>
        <Plus />
        {add.isPending ? "Queueing…" : "Queue work"}
      </Button>
    </form>
  );
}
