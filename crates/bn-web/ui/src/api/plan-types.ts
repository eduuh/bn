export type PlanStatus =
  | "none"
  | "drafting"
  | "ready"
  | "review-pending"
  | "changes-requested"
  | "approved"
  | "executing"
  | "shipped"
  | (string & {});

export interface AgentPlanSummary {
  status: PlanStatus;
  active: string;
  active_exists: boolean;
  drafts: number;
}

export interface PlanFileRef {
  path: string;
  absolute_path?: string;
  exists?: boolean;
  size: number;
  modified?: number | null;
  kind: "dir" | "markdown" | "image" | "video" | "file" | "missing" | (string & {});
  name?: string;
}

export interface NotePlanView {
  repo: string;
  branch: string;
  note_dir: string;
  active: string;
  active_abs: string;
  active_exists: boolean;
  status: PlanStatus;
  drafts: PlanFileRef[];
}

export interface NoteFilesView {
  path: string;
  absolute_path: string;
  modified?: number | null;
  entries: PlanFileRef[];
}

export interface NoteFileView {
  file: PlanFileRef | null;
  content: string;
}
