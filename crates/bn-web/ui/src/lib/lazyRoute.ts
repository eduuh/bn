import { lazy, type ComponentType, type LazyExoticComponent } from "react";

/** Every route loader registered by [`lazyRoute`], for [`prefetchRoutes`] to warm. */
const LOADERS: Array<() => Promise<unknown>> = [];

/**
 * `React.lazy` for a route module that exports its view by name rather than as a default.
 *
 * Every route in `main.tsx` is `export function Foo()`, which `lazy()` can't consume directly — it
 * insists on a default export. This adapts the named export instead of making twenty route files
 * grow a default one, and keeps the route table readable: one line per route, the dynamic
 * `import()` literal still visible so Vite can see the split point.
 */
export function lazyRoute<K extends string>(
  load: () => Promise<Record<K, ComponentType>>,
  name: K,
): LazyExoticComponent<ComponentType> {
  LOADERS.push(load);
  return lazy(() => load().then((m) => ({ default: m[name] })));
}

/**
 * Pull every route chunk down once the browser goes idle.
 *
 * Splitting the routes bought a small entry chunk but cost two things the single bundle gave for
 * free: the first visit to a route waits on a request, and the service worker (cache-first on
 * hashed assets) only ever caches the routes you actually opened, so an offline phone would find
 * the rest missing. Warming them after first paint gets both back — the chunks are in the SW cache
 * and in memory before you navigate, and none of it competes with the initial render.
 */
export function prefetchRoutes(): void {
  const warm = () => {
    // A chunk that fails here is not an error: the route retries on navigation and surfaces it
    // there. Swallowing keeps a flaky network from logging twenty rejections at startup.
    for (const load of LOADERS) load().catch(() => undefined);
  };
  // Safari only shipped requestIdleCallback recently; a timer is a fine stand-in on older ones.
  if (typeof window.requestIdleCallback === "function") {
    window.requestIdleCallback(warm, { timeout: 5_000 });
  } else {
    window.setTimeout(warm, 2_000);
  }
}
