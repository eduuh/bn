import { Suspense, useEffect, useState } from "react";
import { Outlet, useLocation } from "react-router-dom";

import { useBnStream } from "./hooks/useBnStream";
import { ConnectionBanner } from "./components/ConnectionBanner";
import { CommandPalette } from "./components/CommandPalette";
import { RouteBoundary } from "./components/RouteBoundary";
import { TopBar } from "./components/shell/TopBar";
import { Rail } from "./components/shell/Rail";
import { cn } from "./lib/utils";
import { pageWidth } from "./lib/layout";
import { syncExperimentalFromUrl } from "./lib/experimental";

/**
 * The shell (clay console redesign, #966): a 46px topbar carrying identity and host vitals over a
 * two-column grid of rail + content.
 *
 * What changed from the v2 dashboard: the wordmark and connection dot moved out of the sidebar into
 * a real topbar, which freed the rail to lead with the one verb that matters (**Start an agent**)
 * instead of a search box; and the phone shell lost its hamburger drawer entirely — the rail *is*
 * the bottom tab bar there, so all fifteen destinations are one tap away rather than eleven of them
 * sitting one tap behind a drawer.
 *
 * One `useBnStream` still owns the `EventSource` and patches the react-query cache, so every count
 * in the rail and every meter in the topbar stays live without either component polling.
 */
export function App() {
  const { status } = useBnStream();
  const [paletteOpen, setPaletteOpen] = useState(false);
  const location = useLocation();

  // ⌘K / Ctrl-K toggles the global command palette from anywhere in the shell (#565).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setPaletteOpen((v) => !v);
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);

  // `?experimental=1` reveals the thirteen destinations that are still being tested; `=0` hides
  // them again. Read on every navigation, not just at load, because React Router owns the URL once
  // the shell is up (see `lib/experimental`).
  useEffect(() => {
    syncExperimentalFromUrl(location.search);
  }, [location.search]);

  // How wide this route wants to be (see `lib/layout`). `full` routes are the console language —
  // full-bleed, multi-column, scrolling their own panes; `standard` and `document` are page-scrolled
  // measures, fluid and narrow respectively.
  const width = pageWidth(location.pathname);
  const bare = width === "full";

  return (
    <div className="flex h-[100dvh] flex-col overflow-hidden">
      <TopBar status={status} onOpenPalette={() => setPaletteOpen(true)} />

      <div className="flex min-h-0 flex-1 flex-col md:grid md:grid-cols-[12.5rem_minmax(0,1fr)]">
        <Rail />

        <div className="flex min-h-0 flex-1 flex-col md:min-w-0">
          <ConnectionBanner status={status} />
          <main className={cn("min-h-0 flex-1", bare ? "flex flex-col" : "overflow-y-auto")}>
            {/* Every route but the index is a lazy chunk (see `main.tsx`). Off a LAN the chunk is
                there in a frame or two, so the fallback is deliberately quiet — a blank measure
                rather than a spinner that would flash on every navigation. */}
            <RouteBoundary key={location.pathname}>
              <Suspense fallback={null}>
                {bare ? (
                  <Outlet />
                ) : (
                  <div
                    className={cn(
                      "px-3 py-3.5 md:px-6 md:py-5",
                      // `standard` stays fluid to a generous max so the content tracks the topbar
                      // instead of floating in 355px gutters at 1920.
                      //
                      // `document` keeps the narrow measure forms and prose want, but is *not*
                      // centred: centring a narrow column inside the content area put `/start`'s
                      // page title 170px right of the title on every other route, so the heading
                      // jumped every time you navigated. Left-aligned, every page's title lands on
                      // the same x and only the measure changes.
                      width === "document" ? "max-w-[52rem]" : "mx-auto max-w-[100rem]",
                    )}
                  >
                    <Outlet />
                  </div>
                )}
              </Suspense>
            </RouteBoundary>
          </main>
        </div>
      </div>

      <CommandPalette open={paletteOpen} onOpenChange={setPaletteOpen} />
    </div>
  );
}
