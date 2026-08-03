import { Suspense, useEffect, useState } from "react";
import { Outlet, useLocation } from "react-router-dom";

import { useBnStream } from "./hooks/useBnStream";
import { ConnectionBanner } from "./components/ConnectionBanner";
import { CommandPalette } from "./components/CommandPalette";
import { RouteBoundary } from "./components/RouteBoundary";
import { TopBar } from "./components/shell/TopBar";
import { Rail } from "./components/shell/Rail";
import { cn } from "./lib/utils";

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

  // The console is a full-bleed three-column app that manages its own scrolling; every other route
  // is a document on the prototype's 1060px measure.
  // The console is the only full-bleed, self-scrolling screen; since #1006 it lives at
  // `/console` rather than at the index.
  const bare = location.pathname === "/console";

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
                  <div className="mx-auto max-w-[1060px] px-3 py-3.5 md:px-6 md:py-5">
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
