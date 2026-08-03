import { Link, isRouteErrorResponse, useLocation, useRouteError } from "react-router-dom";
import { AlertTriangle, Compass, RotateCw } from "lucide-react";

import { Button } from "../components/ui/button";
import { Card } from "../components/ui/card";

/**
 * The screens that catch a URL or a component going wrong.
 *
 * Before these existed the router declared no catch-all and no `errorElement`, so *anything*
 * unmatched — a stale PWA shortcut, a renamed route, a mistyped path — fell through to React
 * Router's own dev-facing boundary: "Unexpected Application Error! 404 Not Found" as bare text on a
 * blank page, with no nav and no way back. The same boundary swallowed render errors, so one
 * throwing component took the entire dashboard with it.
 *
 * Every route detail screen already degraded properly ([`RepoDetail`]'s "Repo not found" and
 * friends) — only the router itself was unguarded. These three components close that gap at the
 * three levels it can fail: an unknown path, a crashing route, and a crashing shell.
 */

/** An unmatched path. Rendered as a child route, so the rail and topbar stay put. */
export function NotFound() {
  const location = useLocation();
  return (
    <Card className="flex flex-col items-center gap-3 px-6 py-12 text-center">
      <Compass className="h-6 w-6 text-muted-foreground" />
      <h3 className="text-lg font-semibold">Nothing lives here</h3>
      <p className="max-w-md text-sm text-muted-foreground">
        No screen at <code className="font-mono">{location.pathname}</code>. It may have been renamed
        — the rail on the left has every destination, or press{" "}
        <kbd className="rounded border border-border px-1 font-mono text-xs">⌘K</kbd> to search.
      </p>
      <Button asChild size="sm" variant="secondary">
        <Link to="/">Back to the console</Link>
      </Button>
    </Card>
  );
}

/**
 * A route that threw. Rendered inside the shell (the router nests it under a pathless layout
 * route), so one broken screen costs you that screen and not the dashboard.
 */
export function RouteError() {
  const error = useRouteError();
  return (
    <Card className="flex flex-col items-center gap-3 border-urgent/30 bg-urgent-muted px-6 py-12 text-center">
      <AlertTriangle className="h-6 w-6 text-urgent" />
      <h3 className="text-lg font-semibold">This screen failed to render</h3>
      <p className="max-w-md text-sm text-muted-foreground">
        The rest of the dashboard is fine — pick another destination from the rail, or reload to try
        this one again.
      </p>
      <p className="max-w-full overflow-x-auto font-mono text-xs text-muted-foreground">
        {describe(error)}
      </p>
      <div className="flex gap-2">
        <Button size="sm" variant="secondary" onClick={() => window.location.reload()}>
          <RotateCw className="h-4 w-4" /> Reload
        </Button>
        <Button asChild size="sm" variant="secondary">
          <Link to="/">Console</Link>
        </Button>
      </div>
    </Card>
  );
}

/**
 * The shell itself threw, so there is no rail to fall back on — this replaces the whole page. Kept
 * deliberately dependency-light (no shared layout, no queries) because whatever broke may be the
 * thing those rely on.
 */
export function RootError() {
  const error = useRouteError();
  return (
    <div className="flex h-[100dvh] flex-col items-center justify-center gap-3 p-6 text-center">
      <AlertTriangle className="h-6 w-6 text-urgent" />
      <h1 className="text-lg font-semibold">bn/web could not start</h1>
      <p className="max-w-md text-sm text-muted-foreground">
        The app shell failed to render. A reload usually clears it; if it does not, the server may be
        serving a stale bundle.
      </p>
      <p className="max-w-full overflow-x-auto font-mono text-xs text-muted-foreground">
        {describe(error)}
      </p>
      <Button size="sm" variant="secondary" onClick={() => window.location.reload()}>
        <RotateCw className="h-4 w-4" /> Reload
      </Button>
    </div>
  );
}

/** A one-line, human-readable form of whatever the router handed us. */
function describe(error: unknown): string {
  if (isRouteErrorResponse(error)) return `${error.status} ${error.statusText}`;
  if (error instanceof Error) return error.message;
  return String(error ?? "unknown error");
}
