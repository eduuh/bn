import { useEffect, useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { ArrowUp, FileText, Folder, GitBranch, RefreshCw, TriangleAlert } from "lucide-react";
import { noteFileQuery, noteFilesQuery, notePlanQuery } from "../api/client";
import { promotePlanDraft } from "../api/writes";
import type { NotePlanView, PlanFileRef, PlanStatus } from "../api/plan-types";
import { useAppMutation } from "../hooks/useAppMutation";
import MarkdownBody from "./MarkdownBody";
import { Badge } from "./ui/badge";
import { Button } from "./ui/button";
import { Card } from "./ui/card";
import { ScrollArea } from "./ui/scroll-area";
import { Skeleton } from "./ui/skeleton";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "./ui/table";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "./ui/tooltip";

const PLAN_TONE: Record<string, "ok" | "warn" | "urgent" | "muted" | "info"> = {
  none: "muted",
  drafting: "info",
  ready: "warn",
  "review-pending": "warn",
  "changes-requested": "urgent",
  approved: "ok",
  executing: "info",
  shipped: "ok",
};

export function PlanStatusBadge({ status }: { status: PlanStatus }) {
  return <Badge variant={PLAN_TONE[status] ?? "muted"}>plan: {status}</Badge>;
}

export function PlanSummaryCard({ repo, branch }: { repo: string; branch: string }) {
  const plan = useQuery(notePlanQuery(repo, branch));
  if (plan.isLoading) return <Skeleton className="h-28 w-full" />;
  if (plan.isError) {
    return (
      <Card className="flex items-center gap-2 border-urgent/30 bg-urgent-muted p-3 text-sm text-urgent">
        <TriangleAlert className="h-4 w-4" /> Could not load plan artifacts.
      </Card>
    );
  }
  if (!plan.data) return null;
  return <PlanSummary plan={plan.data} />;
}

function PlanSummary({ plan }: { plan: NotePlanView }) {
  const promote = useAppMutation({
    mutationFn: (draft: string) => promotePlanDraft(plan.repo, plan.branch, draft),
    invalidates: [
      ["note", plan.repo, plan.branch],
      ["note", plan.repo, plan.branch, "plan"],
      ["note", plan.repo, plan.branch, "file", "plans/active.md"],
      ["fleet"],
    ],
    toast: { success: "Promoted draft to active plan", error: "Could not promote plan" },
  });
  return (
    <Card className="space-y-3 p-4">
      <div className="flex flex-wrap items-center gap-2">
        <h3 className="text-sm font-semibold uppercase tracking-wide text-muted-foreground">
          Plan artifact
        </h3>
        <PlanStatusBadge status={plan.status} />
        {!plan.active_exists && <Badge variant="urgent">missing active</Badge>}
      </div>
      <div className="grid gap-2 text-sm sm:grid-cols-2">
        <div>
          <div className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
            Active
          </div>
          <code className="break-all font-mono text-xs text-foreground">{plan.active}</code>
        </div>
        <div>
          <div className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
            Drafts
          </div>
          <span className="text-foreground">{plan.drafts.length}</span>
        </div>
      </div>
      {plan.drafts.length > 0 && (
        <div className="space-y-2">
          <div className="flex flex-wrap gap-1.5">
            {plan.drafts.slice(0, 5).map((draft) => (
              <TooltipProvider key={draft.path}>
                <Tooltip>
                  <TooltipTrigger asChild>
                    <Badge variant={draft.exists === false ? "urgent" : "muted"}>
                      <GitBranch className="h-3 w-3" />
                      {draft.path.split("/").pop()}
                    </Badge>
                  </TooltipTrigger>
                  <TooltipContent>{draft.path}</TooltipContent>
                </Tooltip>
              </TooltipProvider>
            ))}
            {plan.drafts.length > 5 && <Badge variant="muted">+{plan.drafts.length - 5}</Badge>}
          </div>
          <div className="flex flex-wrap gap-2">
            {plan.drafts
              .filter((draft) => draft.exists !== false)
              .slice(0, 3)
              .map((draft) => (
                <Button
                  key={draft.path}
                  type="button"
                  size="sm"
                  variant="secondary"
                  disabled={promote.isPending}
                  onClick={() => promote.mutate(draft.path)}
                >
                  Promote {draft.path.split("/").pop()}
                </Button>
              ))}
          </div>
        </div>
      )}
    </Card>
  );
}

export function PlanMarkdownViewer({ repo, branch }: { repo: string; branch: string }) {
  const plan = useQuery(notePlanQuery(repo, branch));
  const active = plan.data?.active_exists ? plan.data.active : "";
  const file = useQuery(noteFileQuery(repo, branch, active));

  if (plan.isLoading || (active && file.isLoading)) {
    return (
      <div className="space-y-2">
        <Skeleton className="h-4 w-1/3" />
        <Skeleton className="h-40 w-full" />
      </div>
    );
  }
  if (plan.isError || file.isError || !file.data) {
    return (
      <Card className="flex items-center gap-2 border-warn/30 bg-warn-muted p-3 text-sm text-warn">
        <TriangleAlert className="h-4 w-4" /> No active plan file to preview yet.
      </Card>
    );
  }
  return (
    <Card className="space-y-3 p-4">
      <div className="flex flex-wrap items-center gap-2">
        <FileText className="h-4 w-4 text-muted-foreground" />
        <code className="break-all font-mono text-xs text-muted-foreground">{active}</code>
      </div>
      <ScrollArea className="max-h-[36rem] pr-3">
        <MarkdownBody text={file.data.content} />
      </ScrollArea>
    </Card>
  );
}

export function BnFileBrowser({ repo, branch }: { repo: string; branch: string }) {
  const plan = useQuery(notePlanQuery(repo, branch));
  const [dir, setDir] = useState("");
  const [selected, setSelected] = useState("");
  const listing = useQuery(noteFilesQuery(repo, branch, dir));
  const file = useQuery(noteFileQuery(repo, branch, selected));

  useEffect(() => {
    if (selected || !plan.data?.active_exists) return;
    setSelected(plan.data.active);
  }, [plan.data?.active, plan.data?.active_exists, selected]);

  const crumbs = useMemo(() => {
    const parts = dir.split("/").filter(Boolean);
    return [{ label: "bn", path: "" }].concat(
      parts.map((part, i) => ({ label: part, path: parts.slice(0, i + 1).join("/") })),
    );
  }, [dir]);

  return (
    <div className="grid grid-cols-1 gap-4 lg:grid-cols-[minmax(0,24rem)_minmax(0,1fr)]">
      <Card className="space-y-3 p-3">
        <div className="flex flex-wrap items-center gap-1.5">
          {crumbs.map((crumb, i) => (
            <Button
              key={crumb.path || "root"}
              type="button"
              size="sm"
              variant={i === crumbs.length - 1 ? "secondary" : "ghost"}
              onClick={() => setDir(crumb.path)}
            >
              {crumb.label}
            </Button>
          ))}
          <Button
            type="button"
            size="sm"
            variant="ghost"
            className="ml-auto"
            onClick={() => {
              void listing.refetch();
            }}
          >
            <RefreshCw className="h-4 w-4" />
          </Button>
          {dir && (
            <Button type="button" size="sm" variant="ghost" onClick={() => setDir(parentDir(dir))}>
              <ArrowUp className="h-4 w-4" />
            </Button>
          )}
        </div>

        {listing.isLoading && <Skeleton className="h-48 w-full" />}
        {listing.isError && (
          <div className="rounded-md border border-urgent/30 bg-urgent-muted p-3 text-sm text-urgent">
            Could not load files.
          </div>
        )}
        {listing.data && (
          <ScrollArea className="max-h-[32rem]">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Name</TableHead>
                  <TableHead className="w-20">Kind</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {listing.data.entries.map((entry) => (
                  <FileRow
                    key={entry.path}
                    entry={entry}
                    onOpenDir={setDir}
                    onOpenFile={setSelected}
                  />
                ))}
              </TableBody>
            </Table>
          </ScrollArea>
        )}
      </Card>

      <Card className="min-h-64 p-4">
        {!selected ? (
          <p className="text-sm text-muted-foreground">Select a file to preview it.</p>
        ) : file.isLoading ? (
          <Skeleton className="h-48 w-full" />
        ) : file.isError || !file.data ? (
          <div className="flex items-center gap-2 text-sm text-warn">
            <TriangleAlert className="h-4 w-4" /> Could not preview {selected}.
          </div>
        ) : (
          <div className="space-y-3">
            <code className="block break-all font-mono text-xs text-muted-foreground">
              {selected}
            </code>
            <ScrollArea className="max-h-[34rem] pr-3">
              {file.data.file?.kind === "markdown" ? (
                <MarkdownBody text={file.data.content} />
              ) : (
                <pre className="whitespace-pre-wrap rounded-md border border-border bg-muted/40 p-3 font-mono text-xs">
                  {file.data.content}
                </pre>
              )}
            </ScrollArea>
          </div>
        )}
      </Card>
    </div>
  );
}

function FileRow({
  entry,
  onOpenDir,
  onOpenFile,
}: {
  entry: PlanFileRef;
  onOpenDir: (path: string) => void;
  onOpenFile: (path: string) => void;
}) {
  const isDir = entry.kind === "dir";
  return (
    <TableRow
      className="cursor-pointer"
      onClick={() => (isDir ? onOpenDir(entry.path) : onOpenFile(entry.path))}
    >
      <TableCell className="max-w-0">
        <div className="flex items-center gap-2">
          {isDir ? (
            <Folder className="h-4 w-4 shrink-0 text-muted-foreground" />
          ) : (
            <FileText className="h-4 w-4 shrink-0 text-muted-foreground" />
          )}
          <span className="truncate font-mono text-xs">{entry.name ?? entry.path}</span>
        </div>
      </TableCell>
      <TableCell>
        <Badge variant="muted">{entry.kind}</Badge>
      </TableCell>
    </TableRow>
  );
}

function parentDir(path: string) {
  const parts = path.split("/").filter(Boolean);
  parts.pop();
  return parts.join("/");
}
