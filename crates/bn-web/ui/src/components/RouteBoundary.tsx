import { Component, type ReactNode } from "react";

/**
 * Catches a route that failed to render — in practice, a lazy chunk that couldn't be fetched.
 *
 * Since the routes are code-split, a tab left open across a `bn-web` restart holds the old build's
 * chunk names; the first navigation after the binary redeploys asks for a file that no longer
 * exists. Without a boundary that rejection unwinds past the shell and leaves a white page, and
 * the fix (reload) isn't discoverable. The boundary keeps the topbar and rail alive and puts the
 * reload in front of you.
 */
export class RouteBoundary extends Component<{ children: ReactNode }, { failed: boolean }> {
  override state = { failed: false };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  override render() {
    if (!this.state.failed) return this.props.children;
    return (
      <div className="mx-auto max-w-[1060px] px-3 py-8 md:px-6">
        <p className="text-sm text-muted-foreground">
          This view didn&rsquo;t load — the dashboard was probably updated while this tab was open.
        </p>
        <button
          type="button"
          onClick={() => window.location.reload()}
          className="mt-3 rounded border border-border px-2.5 py-1 text-sm font-medium hover:bg-muted"
        >
          Reload
        </button>
      </div>
    );
  }
}
