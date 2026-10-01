import { AlertTriangle } from "lucide-react";

import { PageHead } from "../components/PageHead";
import { Card } from "../components/ui/card";

/**
 * `/staff` is still an experimental destination, but the backing read APIs have not landed on the
 * server yet. Keep the route resolvable and explicit instead of letting the page hammer missing
 * endpoints and log 404s on every visit.
 */
export function Staff() {
  return (
    <div className="mx-auto w-full max-w-4xl px-4 py-5">
      <PageHead title="Staff" sub="briefing cards are not wired into bn-web yet" />

      <Card className="space-y-2 border-warn/30 bg-warn-muted p-4 text-sm text-warn">
        <div className="flex items-center gap-2 font-medium">
          <AlertTriangle className="h-4 w-4" />
          Staff briefing cards are not available on this build yet.
        </div>
        <p>
          The dashboard route stays here so saved links and experimental toggles still resolve, but
          the server-side staff board/feed APIs are not implemented. Showing this placeholder is
          intentional: it avoids the repeated 404s the old screen logged on every visit.
        </p>
      </Card>
    </div>
  );
}

export default Staff;
