import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { BookOpenCheck, CircleDot, Loader2, PauseCircle, Pencil } from "lucide-react";

import { queryKeys, staffBoardQuery, staffCardQuery, staffFeedQuery } from "../api/client";
import type { StaffAgentRow, StaffCardRef } from "../api/types";
import { markStaffCardRead, setStaffSchedule } from "../api/writes";
import { Input } from "../components/ui/input";
import { pushToast } from "../lib/toast";
import { ToastStack } from "../components/ToastStack";
import MarkdownBody from "../components/MarkdownBody";
import { PageHead } from "../components/PageHead";
import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
import { Skeleton } from "../components/ui/skeleton";
import { timeAgo } from "../lib/format";
import { cn } from "../lib/utils";

/** The badge tone for the board's one-word health taxonomy (#273): red is a hard problem the
 * schedule owner must fix, amber a transient policy state, muted quiet-by-design, ok a card
 * landed. Mirrors the CLI board's colour map so the two surfaces read the same. */
function healthVariant(health: string): "urgent" | "warn" | "muted" | "ok" {
  switch (health) {
    case "missed":
    case "fail":
    case "auth":
      return "urgent";
    case "rate-limited":
      return "warn";
    case "ok-silent":
    case "skipped":
    case "never":
      return "muted";
    default:
      return "ok";
  }
}

/** The inline `on_calendar` editor: click the schedule to edit it in place, Enter saves, Escape
 * cancels, an emptied field unschedules. The server validates the OnCalendar spec, so a typo
 * comes back as a toast instead of a silently-dead timer. */
function ScheduleCell({ agent }: { agent: StaffAgentRow }) {
  const [editing, setEditing] = useState(false);
  const [value, setValue] = useState(agent.on_calendar ?? "");
  const qc = useQueryClient();
  const save = useMutation({
    mutationFn: (on_calendar: string | null) =>
      setStaffSchedule({ agent: agent.name, on_calendar }),
    onSuccess: (_, on_calendar) => {
      pushToast(
        on_calendar
          ? `${agent.name} scheduled: ${on_calendar}`
          : `${agent.name} unscheduled (manual/trigger only)`,
        "success",
      );
      setEditing(false);
      void qc.invalidateQueries({ queryKey: queryKeys.staffBoard });
    },
    onError: (e: Error) => pushToast(e.message, "error"),
  });
  if (!editing) {
    return (
      <button
        type="button"
        onClick={() => {
          setValue(agent.on_calendar ?? "");
          setEditing(true);
        }}
        className="group flex items-center gap-1 font-mono text-[11.5px] text-muted-foreground hover:text-foreground"
        title="Edit schedule (systemd OnCalendar)"
      >
        {agent.on_calendar ?? "unscheduled"}
        <Pencil className="h-3 w-3 opacity-0 transition-opacity group-hover:opacity-60" />
      </button>
    );
  }
  return (
    <form
      className="flex items-center gap-1.5"
      onSubmit={(e) => {
        e.preventDefault();
        save.mutate(value.trim() === "" ? null : value.trim());
      }}
    >
      <Input
        autoFocus
        value={value}
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={(e) => e.key === "Escape" && setEditing(false)}
        placeholder="Mon..Fri *-*-* 14:50:00 — empty unschedules"
        className="h-6 w-64 font-mono text-[11.5px]"
      />
      <Button type="submit" size="sm" variant="secondary" className="h-6" disabled={save.isPending}>
        {save.isPending ? <Loader2 className="h-3 w-3 animate-spin" /> : "Save"}
      </Button>
    </form>
  );
}

/** One roster row: identity + editable schedule on the left, the health story on the right. The
 * paused badge is the row's most important bit — it names the agents that have stopped writing
 * because their cards sit unread (#269), which reading below resumes. */
function AgentRow({ agent, now }: { agent: StaffAgentRow; now: number }) {
  const when =
    agent.missed && agent.missed_since
      ? `missed ${timeAgo(agent.missed_since, now)} ago`
      : agent.last_ts
        ? `${timeAgo(agent.last_ts, now)} ago`
        : "never run";
  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-border/60 px-3 py-2 last:border-b-0">
      <div className="flex min-w-[11rem] items-center gap-2">
        {agent.unread_card && (
          <CircleDot className="h-3 w-3 flex-none text-info" aria-label="unread card" />
        )}
        <span className="font-medium">{agent.name}</span>
      </div>
      <ScheduleCell agent={agent} />
      <div className="ml-auto flex items-center gap-2">
        {agent.paused && (
          <Badge variant="warn" className="gap-1">
            <PauseCircle className="h-3 w-3" />
            paused · {agent.unread_streak} unread
          </Badge>
        )}
        <Badge variant={healthVariant(agent.health)}>{agent.health}</Badge>
        <span className="w-24 text-right font-mono text-[11.5px] tabular-nums text-muted-foreground">
          {when}
        </span>
      </div>
      {agent.last_error && (
        <div className="w-full pl-5 text-[12px] text-muted-foreground">└ {agent.last_error}</div>
      )}
    </div>
  );
}

/** One feed entry: the headline line expands into the rendered card, and rendering offers the
 * mark-read write — the same read act the CLI's `bn staff feed` performs, and the lever that
 * resumes a paused agent. */
function FeedCard({ card, now }: { card: StaffCardRef; now: number }) {
  const [open, setOpen] = useState(false);
  const qc = useQueryClient();
  const detail = useQuery({ ...staffCardQuery(card.date, card.agent), enabled: open });
  const markRead = useMutation({
    mutationFn: () => markStaffCardRead({ date: card.date, agent: card.agent }),
    onSuccess: () => {
      pushToast(`${card.agent} · ${card.date} marked read`, "success");
      void qc.invalidateQueries({ queryKey: queryKeys.staffFeed });
      void qc.invalidateQueries({ queryKey: queryKeys.staffBoard });
      void qc.invalidateQueries({ queryKey: queryKeys.staffCard(card.date, card.agent) });
    },
    onError: (e: Error) => pushToast(e.message, "error"),
  });
  void now;
  return (
    <div className="border-b border-border/60 last:border-b-0">
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        className={cn(
          "flex w-full flex-wrap items-baseline gap-x-3 gap-y-0.5 px-3 py-2 text-left hover:bg-card",
          !card.read && "font-medium",
        )}
      >
        {!card.read && <CircleDot className="h-3 w-3 flex-none self-center text-info" />}
        <span className="font-mono text-[11.5px] tabular-nums text-muted-foreground">
          {card.date}
        </span>
        <span className="text-[13px] text-muted-foreground">{card.agent}</span>
        <span className="min-w-0 flex-1 truncate text-[13.5px]">{card.headline}</span>
      </button>
      {open && (
        <div className="px-4 pb-3">
          {detail.isLoading && <Skeleton className="h-16 w-full" />}
          {detail.error && (
            <div className="text-[12.5px] text-destructive">{detail.error.message}</div>
          )}
          {detail.data && (
            <>
              <MarkdownBody text={detail.data.markdown} />
              {!card.read && (
                <Button
                  size="sm"
                  variant="secondary"
                  className="mt-2"
                  disabled={markRead.isPending}
                  onClick={() => markRead.mutate()}
                >
                  {markRead.isPending ? (
                    <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />
                  ) : (
                    <BookOpenCheck className="mr-1 h-3.5 w-3.5" />
                  )}
                  Mark read
                </Button>
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
}

/** The staff surface: who is on the roster, whether each agent is healthy / quiet / paused, and
 * the briefing-card feed — readable (and markable read) in place, because unread cards are what
 * pause the whole staff. */
export function Staff() {
  const board = useQuery(staffBoardQuery());
  const feed = useQuery(staffFeedQuery());
  const now = Math.floor(Date.now() / 1000);

  const agents = board.data?.agents ?? [];
  const cards = feed.data?.cards ?? [];
  const unread = cards.filter((c) => !c.read).length;
  const paused = agents.filter((a) => a.paused).length;

  return (
    <div className="mx-auto w-full max-w-4xl px-4 py-5">
      <PageHead
        title="Staff"
        sub={
          board.isLoading
            ? "…"
            : `${agents.length} agents · ${unread} unread card${unread === 1 ? "" : "s"}${
                paused ? ` · ${paused} paused` : ""
              }`
        }
      />

      {paused > 0 && (
        <div className="mb-3 rounded-md border border-warn/30 bg-warn-muted px-3 py-2 text-[13px] text-warn">
          {paused} agent{paused === 1 ? " is" : "s are"} self-paused because their newest cards sit
          unread — reading a card below resumes them on the next scheduled run.
        </div>
      )}

      <Card className="mb-5 overflow-hidden p-0">
        {board.isLoading && <Skeleton className="m-3 h-24" />}
        {board.error && (
          <div className="px-3 py-2 text-[13px] text-destructive">{board.error.message}</div>
        )}
        {agents.map((a) => (
          <AgentRow key={a.name} agent={a} now={now} />
        ))}
        {!board.isLoading && agents.length === 0 && (
          <div className="px-3 py-3 text-[13px] text-muted-foreground">
            No staff agents. Add a .md file with <code>gather:</code> in config/agents/.
          </div>
        )}
      </Card>

      <h3 className="mb-2 font-display text-lg font-semibold tracking-tight">Briefing cards</h3>
      <Card className="overflow-hidden p-0">
        {feed.isLoading && <Skeleton className="m-3 h-24" />}
        {cards.map((c) => (
          <FeedCard key={`${c.date}/${c.agent}`} card={c} now={now} />
        ))}
        {!feed.isLoading && cards.length === 0 && (
          <div className="px-3 py-3 text-[13px] text-muted-foreground">
            No briefing cards yet — the feed fills as scheduled staff runs land.
          </div>
        )}
      </Card>
      <ToastStack />
    </div>
  );
}

export default Staff;
