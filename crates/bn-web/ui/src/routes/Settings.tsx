import { useQuery } from "@tanstack/react-query";
import { FlaskConical, RotateCcw } from "lucide-react";

import { healthQuery } from "../api/client";
import { EXPERIMENTAL_ALL, setExperimental, useExperimental } from "../lib/experimental";
import { EXPERIMENTAL_GROUPS, EXPERIMENTAL_KEYS } from "../lib/nav";
import { PageHead } from "../components/PageHead";
import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";
import { Skeleton } from "../components/ui/skeleton";
import { cn } from "../lib/utils";

/**
 * `/settings` — the preferences this browser holds, and the vitals of the server it is talking to.
 *
 * It exists mainly for the experimental gate. Optional destinations are hidden until
 * `?experimental=` names it (see `lib/experimental`), which meant the only way to reveal one was to
 * already know the flag existed, know the key, and hand-edit the query string — and the only way to
 * see what was currently on was to hover the `exp` chip in the topbar. Here the same set is a list
 * of switches with the tab's own name on it, so the gate is discoverable and reversible.
 *
 * The switches write the same sticky store the query param writes, so the rail re-renders the
 * instant one flips and the choice survives a reload. Nothing on this page is server state: the
 * flag is per-browser by design, so a phone and a laptop can be showing different surfaces.
 */
export function Settings() {
  const experimental = useExperimental();
  const health = useQuery(healthQuery());

  const all = experimental.keys.includes(EXPERIMENTAL_ALL);
  const on = (key: string) => all || experimental.keys.includes(key);
  const enabledCount = all ? EXPERIMENTAL_KEYS.length : EXPERIMENTAL_KEYS.filter(on).length;

  // Keys in storage that no tab claims — a typo in a hand-typed param, or a tab from a newer build.
  // They ride through every write untouched rather than being silently swallowed by a switch that
  // knows nothing about them.
  const extras = all ? [] : experimental.keys.filter((k) => !EXPERIMENTAL_KEYS.includes(k));

  /** Flip one tab. Collapses to the `*` marker once every tab is on, so the topbar chip and a
   * copied `?experimental=` URL both read "all" rather than a fifteen-item list. */
  const toggle = (key: string) => {
    const next = new Set(all ? EXPERIMENTAL_KEYS : [...experimental.keys]);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    const everything = EXPERIMENTAL_KEYS.every((k) => next.has(k));
    setExperimental(everything ? [EXPERIMENTAL_ALL] : [...next, ...extras]);
  };

  return (
    <>
      <PageHead
        title="Settings"
        sub={`${enabledCount}/${EXPERIMENTAL_KEYS.length} experimental tabs on`}
      />

      <Card className="mb-4 p-4">
        <div className="flex flex-wrap items-start gap-x-3 gap-y-2">
          <FlaskConical className="mt-px h-4 w-4 flex-none text-warn" />
          <div className="min-w-0 flex-1">
            <h3 className="font-semibold leading-none tracking-tight">Experimental surfaces</h3>
            <p className="mt-2 text-sm leading-relaxed text-muted-foreground">
              bn ships more screens than have been tested end to end, so the shell defaults to Today
              + Cron and keeps the rest opt-in one at a time. Turning a tab on puts it back in the
              rail and the ⌘K palette; the route itself always works, so a bookmark or a deep link
              resolves either way.
            </p>
            <p className="mt-2 text-sm leading-relaxed text-muted-foreground">
              This is a per-browser preference, not a server setting. It sticks across reloads, and{" "}
              <code className="rounded bg-muted px-1 py-px font-mono text-[12px]">
                ?experimental=fleet,queue
              </code>{" "}
              still overrides it from any URL.
            </p>
          </div>
        </div>

        <div className="mt-3.5 flex flex-wrap items-center gap-2 border-t border-border pt-3.5">
          <Button
            size="sm"
            variant={all ? "secondary" : "default"}
            onClick={() => setExperimental([EXPERIMENTAL_ALL])}
            disabled={all}
          >
            Show everything
          </Button>
          <Button
            size="sm"
            variant="outline"
            onClick={() => setExperimental([])}
            disabled={enabledCount === 0 && extras.length === 0}
          >
            <RotateCcw />
            Back to defaults
          </Button>
          {extras.length > 0 && (
            <span className="font-mono text-[11.5px] text-muted-foreground">
              also set, but matching no tab: {extras.join(", ")}
            </span>
          )}
        </div>
      </Card>

      {EXPERIMENTAL_GROUPS.map((group) => (
        <section key={group.title} className="mb-4">
          <h3 className="mb-1.5 px-1 text-[10.5px] font-semibold uppercase tracking-[0.09em] text-muted-foreground/70">
            {group.title}
          </h3>
          <Card className="divide-y divide-border">
            {group.items.map((item) => {
              const key = item.exp as string;
              return (
                <TabSwitch
                  key={key}
                  Icon={item.Icon}
                  label={item.label}
                  to={item.to}
                  blurb={item.blurb}
                  checked={on(key)}
                  onChange={() => toggle(key)}
                />
              );
            })}
          </Card>
        </section>
      ))}

      <section className="mb-4">
        <h3 className="mb-1.5 px-1 text-[10.5px] font-semibold uppercase tracking-[0.09em] text-muted-foreground/70">
          Server
        </h3>
        <Card className="p-4">
          {health.isPending ? (
            <div className="space-y-2">
              <Skeleton className="h-4 w-2/3" />
              <Skeleton className="h-4 w-1/2" />
              <Skeleton className="h-4 w-3/5" />
            </div>
          ) : health.isError ? (
            <p className="text-sm text-urgent">
              Could not read the server’s health: {health.error.message}
            </p>
          ) : (
            <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-5 gap-y-2 text-sm">
              <Fact label="version" value={health.data.version} />
              <Fact label="machine" value={health.data.machine} />
              <Fact label="uptime" value={uptime(health.data.uptime_secs)} />
              <Fact label="listening" value={`${health.data.web.bind}:${health.data.web.port}`} />
              <Fact label="fleet poll" value={`${health.data.web.fleet_poll_secs}s`} />
              <Fact
                label="ui bundle"
                value={health.data.ui_built ? "embedded in the binary" : "dev server"}
              />
            </dl>
          )}
        </Card>
      </section>
    </>
  );
}

/** One label/value row of the server readout. */
function Fact({ label, value }: { label: string; value: string }) {
  return (
    <>
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="truncate font-mono tabular-nums text-foreground">{value}</dd>
    </>
  );
}

/** One tab's row: what it is, and the switch that reveals it.
 *
 * The whole row is the control — a 44px-tall target that works with a thumb — so the switch itself
 * is presentational and the label does not need a `for`/`id` pair to be clickable. */
function TabSwitch({
  Icon,
  label,
  to,
  blurb,
  checked,
  onChange,
}: {
  Icon: React.ComponentType<{ className?: string }>;
  label: string;
  to: string;
  blurb?: string;
  checked: boolean;
  onChange: () => void;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      onClick={onChange}
      className="flex w-full items-center gap-3 px-4 py-3 text-left transition-colors hover:bg-muted/40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background"
    >
      <Icon
        className={cn(
          "h-[18px] w-[18px] flex-none",
          checked ? "text-primary" : "text-muted-foreground/60",
        )}
      />
      <span className="min-w-0 flex-1">
        <span className="flex flex-wrap items-baseline gap-x-2">
          <span className="text-sm font-medium text-foreground">{label}</span>
          <span className="font-mono text-[11px] text-muted-foreground/70">{to}</span>
        </span>
        {blurb && (
          <span className="mt-0.5 block text-[12.5px] leading-snug text-muted-foreground">
            {blurb}
          </span>
        )}
      </span>
      <span
        aria-hidden
        className={cn(
          "relative h-[22px] w-[38px] flex-none rounded-full transition-colors",
          checked ? "bg-primary" : "bg-muted",
        )}
      >
        <span
          className={cn(
            "absolute top-[3px] h-4 w-4 rounded-full bg-background shadow-card transition-[left]",
            checked ? "left-[19px]" : "left-[3px]",
          )}
        />
      </span>
    </button>
  );
}

/** Uptime as the coarsest unit that still says something: "3d 4h", "18m", "42s". */
function uptime(secs: number): string {
  if (secs < 60) return `${secs}s`;
  const m = Math.floor(secs / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ${m % 60}m`;
  return `${Math.floor(h / 24)}d ${h % 24}h`;
}
