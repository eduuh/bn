import React from "react";
import ReactDOM from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createBrowserRouter, Navigate, RouterProvider } from "react-router-dom";
import { App } from "./App";
import { Home } from "./routes/Home";
import { lazyRoute, prefetchRoutes } from "./lib/lazyRoute";
// The 404 and the two error boundaries stay in the entry chunk on purpose. They are the screens
// that render when something has already gone wrong — including a chunk that failed to load — so
// making them lazy would risk needing the network to explain that the network broke. They are a few
// hundred bytes.
import { NotFound, RootError, RouteError } from "./routes/NotFound";
// Every route but the console is lazy. The console is the index route, so it stays in the entry
// chunk and paints without a fallback; the other nineteen views — and the libraries only they pull
// in, uPlot for the cost charts, the markdown renderer for notes and packets — arrive on
// navigation. Bundled eagerly they made a single ~850 kB entry chunk.
// The console is no longer the entry point (#1006) — the task-first home is — so it becomes a
// lazy chunk like every other destination.
const Console = lazyRoute(() => import("./routes/Console"), "Console");
const CommandCenter = lazyRoute(() => import("./routes/CommandCenter"), "CommandCenter");
const Today = lazyRoute(() => import("./routes/Today"), "Today");
const Fleet = lazyRoute(() => import("./routes/Fleet"), "Fleet");
const Nodes = lazyRoute(() => import("./routes/Nodes"), "Nodes");
const Start = lazyRoute(() => import("./routes/Start"), "Start");
const Insights = lazyRoute(() => import("./routes/Insights"), "Insights");
const AgentDetail = lazyRoute(() => import("./routes/AgentDetail"), "AgentDetail");
const TaskDetail = lazyRoute(() => import("./routes/TaskDetail"), "TaskDetail");
const NotesPage = lazyRoute(() => import("./routes/NotesPage"), "NotesPage");
const Handoff = lazyRoute(() => import("./routes/Handoff"), "Handoff");
const Attention = lazyRoute(() => import("./routes/Attention"), "Attention");
const Goals = lazyRoute(() => import("./routes/Goals"), "Goals");
const Cron = lazyRoute(() => import("./routes/Cron"), "Cron");
const CronDetail = lazyRoute(() => import("./routes/CronDetail"), "CronDetail");
const Skills = lazyRoute(() => import("./routes/Skills"), "Skills");
const Research = lazyRoute(() => import("./routes/Research"), "Research");
const Repos = lazyRoute(() => import("./routes/Repos"), "Repos");
const RepoDetail = lazyRoute(() => import("./routes/RepoDetail"), "RepoDetail");
// Roboto, self-hosted. The dashboard is embedded in the binary and served on a LAN (often with no
// route to the internet), so a Google Fonts <link> would silently fall back to system UI. These
// bundle the woff2 into `ui/dist`, which `build.rs` bakes into the binary alongside the JS.
import "@fontsource-variable/roboto";
import "@fontsource-variable/roboto-mono";
import "./index.css";
import "./tailwind.css";

// Live updates flow from the SSE stream into the cache, so queries can stay fresh a while; the
// per-query `refetchInterval` is only a fallback if a frame is ever missed.
const queryClient = new QueryClient({
  defaultOptions: {
    queries: { staleTime: 5_000, refetchOnWindowFocus: false, retry: 1 },
  },
});

// The shell is guarded at three levels (#: live sweep). `RootError` replaces the page only if the
// shell itself throws; the pathless layout route below owns `RouteError`, so a single screen
// blowing up renders inside the rail instead of taking the dashboard down; and the trailing `*`
// catches unmatched paths, which previously fell through to React Router's bare "Unexpected
// Application Error!" page with no nav and no way home.
const router = createBrowserRouter([
  {
    path: "/",
    element: <App />,
    errorElement: <RootError />,
    children: [
      {
        errorElement: <RouteError />,
        children: [
          // `/` is the task-first home (#1006). Work starts from a task or it does not start.
          { index: true, element: <Home /> },
          { path: "console", element: <Console /> },
          // The classic action list keeps its home for the surfaces the console doesn't own yet
          // (the automation hub + the completed history).
          { path: "inbox", element: <CommandCenter /> },
          { path: "today", element: <Today /> },
          { path: "fleet", element: <Fleet /> },
          { path: "nodes", element: <Nodes /> },
          { path: "start", element: <Start /> },
          { path: "reviews", element: <Navigate to="/inbox?tab=reviews" replace /> },
          // Cost merged into the insights page as a tab — the old URL keeps working.
          { path: "cost", element: <Navigate to="/insights?tab=cost" replace /> },
          { path: "insights", element: <Insights /> },
          { path: "agents/:id", element: <AgentDetail /> },
          { path: "tasks/:id", element: <TaskDetail /> },
          { path: "notes", element: <NotesPage /> },
          { path: "handoff", element: <Handoff /> },
          { path: "attention", element: <Attention /> },
          { path: "goals", element: <Goals /> },
          { path: "cron", element: <Cron /> },
          { path: "cron/:id", element: <CronDetail /> },
          { path: "skills", element: <Skills /> },
          { path: "research", element: <Research /> },
          { path: "repos", element: <Repos /> },
          { path: "repos/:repo", element: <RepoDetail /> },
          // Anything else is a stale link or a typo — a real screen, inside the shell.
          { path: "*", element: <NotFound /> },
        ],
      },
    ],
  },
]);

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  </React.StrictMode>,
);

// The console is on screen; spend the idle time pulling the other routes into the (service worker)
// cache so navigation is instant and an offline phone still has every view.
prefetchRoutes();

// PWA: register the service worker so the dashboard is installable and the app shell works on a
// flaky mobile connection (the SW is network-first for `/api`, so live data is never stale-served).
if ("serviceWorker" in navigator && import.meta.env.PROD) {
  window.addEventListener("load", () => {
    navigator.serviceWorker.register("/sw.js").catch(() => {
      /* SW is a progressive enhancement — the dashboard works without it */
    });
  });
}
