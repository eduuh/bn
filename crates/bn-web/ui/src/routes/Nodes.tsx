import { useQuery } from "@tanstack/react-query";
import { Loader2, ServerCrash } from "lucide-react";

import { nodesQuery } from "../api/client";
import type { NodeView } from "../api/types";
import { NodeCard } from "../components/NodeCard";
import { Card } from "../components/ui/card";
import { PageHead } from "../components/PageHead";

/**
 * The nodes view (#966): every machine the fleet runs on, one card each — identity, reachability,
 * vitals, and load. Today the API reports a single node (the local host); the surface is built
 * against the list so remote nodes appear here the moment the backend registers them, with an
 * unreachable state that reads in the signal scale rather than as stale numbers.
 */
export function Nodes() {
  const q = useQuery(nodesQuery());
  const nodes: NodeView[] = q.data?.nodes ?? [];
  const down = nodes.filter((n) => n.status === "unreachable").length;

  return (
    <div className="space-y-5 pb-8">
      <PageHead
        title="Nodes"
        sub={
          q.isLoading
            ? "…"
            : `${nodes.length - down} up${down > 0 ? ` · ${down} unreachable` : ""} · the machines the fleet runs on`
        }
      />

      {q.isLoading && (
        <div className="flex items-center gap-2 text-sm text-muted-foreground" role="status">
          <Loader2 className="h-4 w-4 animate-spin" /> Loading nodes…
        </div>
      )}

      {q.isError && (
        <Card className="flex items-center gap-2 border-urgent/40 p-3 text-sm text-urgent">
          <ServerCrash className="h-4 w-4 flex-none" />
          Could not read the node list — is the server up?
        </Card>
      )}

      {!q.isLoading && !q.isError && (
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          {nodes.map((n) => (
            <NodeCard key={n.host} node={n} />
          ))}
        </div>
      )}

      {!q.isLoading && !q.isError && nodes.length === 1 && (
        <p className="text-xs text-muted-foreground/70">
          One node — this host. Remote nodes appear here automatically once they register with the
          fleet.
        </p>
      )}
    </div>
  );
}
