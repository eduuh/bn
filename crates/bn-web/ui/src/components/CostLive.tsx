import { useQuery } from "@tanstack/react-query";
import { fleetQuery } from "../api/client";
import { fmtCacheReadWrite, fmtTokens } from "../lib/format";
import { StatGrid, StatTile } from "./StatTile";
import { Badge } from "./ui/badge";
import { Card } from "./ui/card";

/**
 * The live in-flight section (#391 §4(b) "live"): run-so-far token usage of the currently-running
 * fleet, straight from the SSE `fleet` frame — the `['fleet']` cache `useBnStream` patches in the
 * shell — so it updates without a reload. No $ / AIC yet: those land only once a run finishes and
 * is ingested into the cost fold (`bn cost`).
 */
export function CostLive() {
  const fleet = useQuery(fleetQuery());
  const agents = fleet.data?.agents ?? [];
  const totalTokens = fleet.data?.total_tokens ?? 0;
  const active = fleet.data?.active ?? 0;

  return (
    <section className="space-y-3">
      <div className="space-y-1">
        <h3 className="text-sm font-semibold uppercase tracking-wide text-muted-foreground">
          Live · in-flight usage
        </h3>
        <p className="text-xs text-muted-foreground">run-so-far tokens · updates without reload</p>
      </div>

      {/* Gate the tiles + empty-state behind the pending signal so a cold load shows "loading",
          not a misleading 0-token / "no agents running" snapshot that then snaps to real values (#544). */}
      {fleet.isLoading ? (
        <p className="text-sm text-muted-foreground">Loading live usage…</p>
      ) : (
        <>
          <StatGrid cols={3} className="border-y border-border">
            <StatTile label="in-flight tokens" value={fmtTokens(totalTokens)} tone="blue" />
            <StatTile
              label="running agents"
              value={String(active)}
              tone={active > 0 ? "green" : "grey"}
            />
          </StatGrid>

          {agents.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              No agents currently running. $ / AIC land once a run finishes and is ingested (bn
              cost).
            </p>
          ) : (
            <Card className="-mx-4 overflow-x-auto rounded-none border-x-0 px-4 py-2 sm:mx-0 sm:rounded-lg sm:border-x sm:px-4">
              <table className="w-full min-w-[24rem] border-collapse text-sm">
                <thead>
                  <tr className="border-b border-border text-xs uppercase tracking-wide text-muted-foreground">
                    <th className="py-2 pr-3 text-left font-medium">Agent</th>
                    <th className="py-2 pl-3 text-right font-medium">Output</th>
                    <th className="py-2 pl-3 text-right font-medium">Cache r/w</th>
                  </tr>
                </thead>
                <tbody>
                  {agents.map((a) => (
                    <tr key={a.session_id} className="border-b border-border/60 last:border-0">
                      <td className="py-2 pr-3">
                        <span className="font-mono text-foreground">
                          {a.repo}:{a.branch}
                        </span>{" "}
                        <Badge variant="muted">{a.harness}</Badge>
                      </td>
                      <td className="py-2 pl-3 text-right font-mono tabular-nums">
                        {fmtTokens(a.output_tokens)}
                      </td>
                      <td className="py-2 pl-3 text-right font-mono tabular-nums text-muted-foreground">
                        {fmtCacheReadWrite(a.cache_read_tokens, a.cache_write_tokens ?? 0)}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </Card>
          )}
        </>
      )}
    </section>
  );
}
