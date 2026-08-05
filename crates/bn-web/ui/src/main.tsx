import React from "react";
import ReactDOM from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createBrowserRouter, Navigate, RouterProvider } from "react-router-dom";
import { App } from "./App";
import { CommandCenter } from "./routes/CommandCenter";
import { Today } from "./routes/Today";
import { Fleet } from "./routes/Fleet";
import { Start } from "./routes/Start";
import { CostView } from "./routes/CostView";
import { Insights } from "./routes/Insights";
import { AgentDetail } from "./routes/AgentDetail";
import { NotesPage } from "./routes/NotesPage";
import { Handoff } from "./routes/Handoff";
import { Attention } from "./routes/Attention";
import { Goals } from "./routes/Goals";
import { Cron } from "./routes/Cron";
import { CronDetail } from "./routes/CronDetail";
import { Skills } from "./routes/Skills";
import { Research } from "./routes/Research";
import { Repos } from "./routes/Repos";
import { RepoDetail } from "./routes/RepoDetail";
import "uplot/dist/uPlot.min.css";
import "./index.css";
import "./tailwind.css";

// Live updates flow from the SSE stream into the cache, so queries can stay fresh a while; the
// per-query `refetchInterval` is only a fallback if a frame is ever missed.
const queryClient = new QueryClient({
  defaultOptions: {
    queries: { staleTime: 5_000, refetchOnWindowFocus: false, retry: 1 },
  },
});

const router = createBrowserRouter([
  {
    path: "/",
    element: <App />,
    children: [
      { index: true, element: <CommandCenter /> },
      { path: "today", element: <Today /> },
      { path: "fleet", element: <Fleet /> },
      { path: "start", element: <Start /> },
      { path: "reviews", element: <Navigate to="/?tab=reviews" replace /> },
      { path: "cost", element: <CostView /> },
      { path: "insights", element: <Insights /> },
      { path: "agents/:id", element: <AgentDetail /> },
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

// PWA: register the service worker so the dashboard is installable and the app shell works on a
// flaky mobile connection (the SW is network-first for `/api`, so live data is never stale-served).
if ("serviceWorker" in navigator && import.meta.env.PROD) {
  window.addEventListener("load", () => {
    navigator.serviceWorker.register("/sw.js").catch(() => {
      /* SW is a progressive enhancement — the dashboard works without it */
    });
  });
}
