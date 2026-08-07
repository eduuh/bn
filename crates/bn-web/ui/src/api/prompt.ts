// The supervision surface: the dialog an agent is blocked on, and the calls that answer it.
//
// Types + read query + write wrappers live together here rather than being split across
// `types.ts` / `client.ts` / `writes.ts`, because they are one small contract and those files are
// at their file-size ratchet.

import type { Envelope } from "./types";

/** What picking an option means, independent of how the harness worded it. */
export type PromptSemantic =
  "approve" | "approve-session" | "reject" | "reject-with-feedback" | "other";

export interface PromptOption {
  /** The digit that picks it. */
  number: number;
  /** The harness's own wording — render this, never a label we invented. */
  label: string;
  semantic: PromptSemantic;
  /** Whether the harness's cursor currently sits on this option. */
  selected: boolean;
}

export interface PendingPrompt {
  kind: "trust-menu" | "command-approval" | "question-menu";
  question: string;
  subject?: string;
  options: PromptOption[];
  /** Echoed back on every answer. The server refuses the answer if the pane has moved since this
   * read, so a tab left open cannot approve a dialog nobody looked at. */
  fingerprint: number;
  excerpt: string[];
}

export interface FleetPrompt {
  target: string;
  session_id: string;
  prompt: PendingPrompt;
}

/** The `Button` variant for each semantic, so approve and reject never look alike. */
export function semanticTone(s: PromptSemantic): "default" | "destructive" | "secondary" {
  if (s === "approve" || s === "approve-session") return "default";
  if (s === "reject" || s === "reject-with-feedback") return "destructive";
  return "secondary";
}

/** A short human gloss for the semantic chip beside each option. */
export function semanticLabel(s: PromptSemantic): string {
  switch (s) {
    case "approve":
      return "allows once";
    case "approve-session":
      return "allows, stops asking";
    case "reject":
      return "declines";
    case "reject-with-feedback":
      return "declines, then asks you to explain";
    default:
      return "";
  }
}

/** `GET /api/agents/:target/prompt`. Resolves to `null` when the agent is not blocked on a dialog
 * — a `no_pending_prompt` envelope is the normal answer, not a failure to surface. */
export async function fetchPrompt(target: string): Promise<PendingPrompt | null> {
  const res = await fetch(`/api/agents/${encodeURIComponent(target)}/prompt`);
  const env = (await res.json()) as Envelope<{ prompt: PendingPrompt }>;
  return env.ok ? (env.data?.prompt ?? null) : null;
}

export function promptQuery(target: string, enabled = true) {
  return {
    queryKey: ["agent-prompt", target] as const,
    queryFn: () => fetchPrompt(target),
    // A blocked agent is the case where latency is felt — you are waiting to unblock it — but a
    // tmux capture per tick is not free, so this is brisk rather than instant.
    refetchInterval: 5_000,
    enabled,
  };
}

export function fleetPromptsQuery() {
  return {
    queryKey: ["fleet-prompts"] as const,
    queryFn: async (): Promise<FleetPrompt[]> => {
      const res = await fetch("/api/fleet/prompts");
      const env = (await res.json()) as Envelope<{ prompts: FleetPrompt[] }>;
      return env.ok ? (env.data?.prompts ?? []) : [];
    },
    refetchInterval: 8_000,
  };
}

/** The outcome of an answer. `ok:false` with `prompt_moved` is an expected result — the caller
 * should re-read and show the new dialog, not retry. */
export interface AnswerResult {
  ok: boolean;
  code?: string;
  message?: string;
  verified?: boolean;
  option?: number;
  label?: string;
}

/** `POST /api/agents/:target/answer`. Always send the fingerprint you read the dialog at. */
export async function answerPrompt(
  target: string,
  choice: { option?: number; semantic?: PromptSemantic },
  fingerprint: number,
): Promise<AnswerResult> {
  const res = await fetch(`/api/agents/${encodeURIComponent(target)}/answer`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ ...choice, fingerprint }),
  });
  const env = (await res.json()) as Envelope<{
    verified?: boolean;
    option?: number;
    label?: string;
  }>;
  return {
    ok: env.ok,
    code: env.error?.code ?? undefined,
    message: env.error?.message ?? undefined,
    verified: env.data?.verified,
    option: env.data?.option,
    label: env.data?.label,
  };
}

/** `POST /api/agents/:target/instruct` — record a standing instruction and deliver it. */
export async function instructAgent(target: string, text: string): Promise<void> {
  const res = await fetch(`/api/agents/${encodeURIComponent(target)}/instruct`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ text }),
  });
  const env = (await res.json()) as Envelope<unknown>;
  if (!env.ok) throw new Error(env.error?.message ?? "instruct failed");
}
