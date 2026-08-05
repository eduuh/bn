// Deterministic fixtures for the bn-web E2E suite (#479 / absorbs #477).
//
// `seed()` populates an isolated runtime dir (state / config / notes / home / bin) that a real
// bn-web binary is booted against, so the dashboard renders and mutates real seeded data — no
// tmux/git/gh/live agents required:
//   * BN_STATE_DIR  → orchestrate-<repo>.json (in-flight fleet) + plan-reviews.json (the inbox)
//   * BN_NOTES_DIR  → <host>/metrics/events.jsonl (agent_session rows the Cost fold reads)
//   * BN_CONFIG_DIR → a pre-written web-token (so the token is a known constant, no --show-token)
//   * BN_BIN        → a stub `bn` that returns success envelopes for the process writes
//                     (dispatch / agent send·relaunch·kill / wt merge), echoing its argv so a test
//                     can assert the exact command the server issued.
// Timestamps are written relative to "now" at seed time, so `timeAgo` reads naturally ("2m ago").

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));

/** Fixed loopback port + known token — shared by the server boot and the specs. */
export const PORT = 7788;
export const HOST = "127.0.0.1";
export const BASE_URL = `http://${HOST}:${PORT}`;
export const TOKEN = "e2e-test-token-000000000000000000000000";
export const HOSTNAME = "e2e-host";

/** The isolated runtime tree (gitignored). Shared by setup + teardown + specs. */
export const RUNTIME_DIR = path.join(here, ".runtime");
export const PID_FILE = path.join(RUNTIME_DIR, "server.pid");
export const LOG_FILE = path.join(RUNTIME_DIR, "server.log");

function write(p: string, contents: string): void {
  fs.mkdirSync(path.dirname(p), { recursive: true });
  fs.writeFileSync(p, contents);
}

/** Resolve the bn-web binary: `$BN_WEB_BIN`, else the workspace debug/release build. */
export function resolveBinary(): string {
  const explicit = process.env.BN_WEB_BIN;
  if (explicit && fs.existsSync(explicit)) return explicit;
  // e2e → ui → bn-web → crates → <repo root> → target/<profile>/bn-web
  const debug = path.resolve(here, "../../../../target/debug/bn-web");
  const release = path.resolve(here, "../../../../target/release/bn-web");
  if (fs.existsSync(debug)) return debug;
  if (fs.existsSync(release)) return release;
  throw new Error(
    `bn-web binary not found. Build it first: cargo build -p bn-web (looked at ${debug}). Or set $BN_WEB_BIN.`,
  );
}

/** The environment a bn-web boot needs to read the seeded runtime tree. */
export function serverEnv(): NodeJS.ProcessEnv {
  return {
    ...process.env,
    BN_STATE_DIR: path.join(RUNTIME_DIR, "state"),
    BN_CONFIG_DIR: path.join(RUNTIME_DIR, "config"),
    BN_NOTES_DIR: path.join(RUNTIME_DIR, "notes"),
    BN_HOSTNAME: HOSTNAME,
    BN_BIN: path.join(RUNTIME_DIR, "bin", "bn"),
    // Deterministic cron + skills tiers (#483): the declared-cron manifest and the skill files the
    // server discovers live in the runtime tree, so `/api/cron` + `/api/skills` are hermetic.
    BN_JOBS_DIR: path.join(RUNTIME_DIR, "jobs"),
    BN_MCP_SKILLS_DIR: path.join(RUNTIME_DIR, "skills"),
    BN_MCP_PERSONAL_SKILLS_DIR: path.join(RUNTIME_DIR, "skills-personal"),
    // Managed repos (#495): `bn` has seeded notes/agents; `forge` is managed-but-empty (its card
    // still renders on the Repos view even with no sessions).
    BN_MANAGED_REPOS: "git@github.com:eduuh/bn.git, https://github.com/eduuh/forge.git",
    HOME: path.join(RUNTIME_DIR, "home"),
  };
}

const STUB_BN = `#!/usr/bin/env bash
# E2E stub bn — echo a success --json envelope for the subcommand bn-web invokes.
args="$*"
sub="$2"; [ "$1" = "--json" ] || sub="$1"
if printf '%s' "$args" | grep -q pane; then
  printf '{"ok":true,"command":"agent","data":{"target":"bn:stub","lines":["stub pane line 1","stub pane line 2"],"count":2},"error":null}\\n'
  exit 0
fi
printf '{"ok":true,"command":"%s","data":{"stub":true,"args":"%s"},"error":null}\\n' "$sub" "$args"
`;

/** Populate the runtime tree with deterministic, freshly-timestamped fixtures. */
export function seed(): void {
  // Reset only the data subtrees — never RUNTIME_DIR itself, so the server pid/log survive a
  // mid-run reseed (specs call this in `beforeEach` to isolate mutable read state).
  fs.mkdirSync(RUNTIME_DIR, { recursive: true });
  for (const d of ["state", "config", "notes", "home", "bin", "jobs", "skills", "skills-personal"]) {
    fs.rmSync(path.join(RUNTIME_DIR, d), { recursive: true, force: true });
  }

  const state = path.join(RUNTIME_DIR, "state");
  const config = path.join(RUNTIME_DIR, "config");
  const notes = path.join(RUNTIME_DIR, "notes");
  const home = path.join(RUNTIME_DIR, "home");
  fs.mkdirSync(path.join(home, ".claude", "projects"), { recursive: true });
  fs.mkdirSync(path.join(home, ".copilot", "session-state"), { recursive: true });

  const now = Math.floor(Date.now() / 1000);
  const ago = (secs: number) => now - secs;
  const day = 86400;

  // Known web token (bn-web's ensure_token reuses a non-empty file → this exact value).
  write(path.join(config, "web-token"), TOKEN);

  // --- Orchestrate run: the in-flight fleet + queue/merged/failed for the count strip. ---
  const orchestrate = {
    repo: "bn",
    concurrency: 3,
    started_at: ago(3 * 3600),
    updated_at: ago(30),
    queue: [
      { id: "500", item: "500", repo: "bn", status: "dispatched", branch: "dispatch/web-redesign-500", harness: "claude", last_state: "working", dispatched_at: ago(1800), updated_at: ago(40) },
      { id: "501", item: "501", repo: "bn", status: "dispatched", branch: "dispatch/plan-review-501", harness: "copilot", last_state: "idle-with-wip", dispatched_at: ago(2400), updated_at: ago(300) },
      { id: "505", item: "505", repo: "bn", status: "dispatched", branch: "dispatch/dns-505", harness: "claude", last_state: "dns-outage", dispatched_at: ago(1200), updated_at: ago(120) },
      { id: "502", item: "add a cost sparkline to the fleet cards", repo: "bn", status: "queued", updated_at: ago(1000) },
      { id: "506", item: "investigate flaky nvim status_spec", repo: "bn", status: "queued", updated_at: ago(1100) },
      { id: "503", item: "503", repo: "bn", status: "merged", branch: "dispatch/land-503", pr: "https://github.com/eduuh/bn/pull/503", updated_at: ago(1800) },
      { id: "504", item: "504", repo: "bn", status: "failed", branch: "dispatch/oops-504", updated_at: ago(2600) },
    ],
    recovery: [
      { ts: ago(1800), id: "503", state: "done", action: "land", note: "merged PR #503" },
      { ts: ago(1200), id: "505", state: "dns-outage", action: "leave", note: "network blip — will retry" },
    ],
  };
  write(path.join(state, "orchestrate-bn.json"), JSON.stringify(orchestrate, null, 2));

  // --- Live orchestrator agent (#500): a long-lived copilot session running the wave, tagged as
  // `orchestration` work so `supervise` stamps its role=orchestrator and the Fleet pins it in the
  // distinct top tier. A live copilot session is a dir under ~/.copilot/session-state with a fresh
  // events.jsonl (so it reads as running) + workspace.yaml (attribution to bn:orchestrator). ---
  const iso = (secs: number) => new Date(ago(secs) * 1000).toISOString();
  const orchSid = "orch-monitor-500";
  const orchDir = path.join(home, ".copilot", "session-state", orchSid);
  write(
    path.join(orchDir, "events.jsonl"),
    [
      JSON.stringify({ type: "session.start", timestamp: iso(300), data: { context: { cwd: "/orch", branch: "orchestrator", repository: "bn" } } }),
      JSON.stringify({ type: "assistant.turn_start", timestamp: iso(90) }),
      JSON.stringify({ type: "assistant.message", timestamp: iso(90), data: { outputTokens: 512 } }),
      JSON.stringify({ type: "tool.execution_start", timestamp: iso(60) }),
    ].join("\n") + "\n",
  );
  write(path.join(orchDir, "workspace.yaml"), `id: ${orchSid}\ncwd: /orch\nbranch: orchestrator\nrepository: bn\n`);

  // --- Plan-review inbox: two pending (r1 ties to item 501), one decided (history). ---
  const reviews = {
    reviews: [
      {
        id: "r1",
        repo: "bn",
        branch: "dispatch/plan-review-501",
        session: "sess-501",
        plan: [
          "## Plan — surface cost in the fleet cards (#501)",
          "",
          "Approach:",
          "1. Add a small sparkline of output-tokens to each fleet card.",
          "2. Reuse the existing Sparkline component + cost fold.",
          "",
          "Files to touch:",
          "- crates/bn-web/ui/src/components/FleetEntryCard.tsx",
          "- crates/bn-web/ui/src/lib/fleet.ts",
          "",
          "Risks: none — additive, behind the existing token flow.",
        ].join("\n"),
        status: "pending",
        created: ago(300),
        updated: ago(300),
      },
      {
        id: "r3",
        repo: "bn",
        branch: "dispatch/refactor-cost-507",
        session: "sess-507",
        plan: [
          "## Plan — refactor cost.rs sampling (#507)",
          "",
          "Extract `sample_from_agent_session` into a testable pure fn and add golden tests.",
          "Files: crates/bn-core/src/cost.rs",
        ].join("\n"),
        status: "pending",
        created: ago(200),
        updated: ago(200),
      },
      {
        id: "r2",
        repo: "bn",
        branch: "dispatch/old-490",
        plan: "Older approved plan for #490.",
        status: "approved",
        created: ago(5000),
        updated: ago(4800),
      },
    ],
  };
  write(path.join(state, "plan-reviews.json"), JSON.stringify(reviews, null, 2));

  // --- Cost: agent_session events under <notes>/<host>/metrics/events.jsonl. ---
  const ev = (o: Record<string, unknown>) => JSON.stringify({ kind: "agent_session", ...o });
  const sess = (
    i: number,
    tsAgo: number,
    work_class: string,
    input: number,
    cache: number,
    output: number,
    aic: number,
  ) =>
    ev({
      ts: ago(tsAgo),
      session_id: `s${i}`,
      repo: "bn",
      harness: "copilot",
      models: "claude-opus-4.8",
      work_class,
      input_tokens: input,
      cache_read_tokens: cache,
      output_tokens: output,
      cost_nanoaiu: Math.round(aic * 1e9),
    });
  const metrics = [
    sess(1, day * 4 + 3600, "coding", 4_200_000, 3_980_000, 120_000, 3.1),
    sess(2, day * 3 + 3600, "coding", 3_100_000, 2_900_000, 88_000, 2.2),
    sess(3, day * 3 + 1800, "orchestration", 9_800_000, 9_500_000, 41_000, 4.8),
    sess(4, day * 2 + 3600, "coding", 5_600_000, 5_300_000, 143_000, 3.9),
    sess(5, day * 2 + 1200, "research", 1_400_000, 1_200_000, 62_000, 1.1),
    sess(6, day * 1 + 3600, "scheduled", 800_000, 700_000, 12_000, 0.4),
    sess(7, day * 1 + 900, "coding", 6_200_000, 5_900_000, 168_000, 4.4),
    sess(8, 3600, "coding", 2_100_000, 1_950_000, 74_000, 1.7),
    sess(9, 1800, "orchestration", 7_400_000, 7_100_000, 33_000, 3.6),
  ].join("\n");
  write(path.join(notes, HOSTNAME, "metrics", "events.jsonl"), metrics + "\n");

  // --- Branch notes (#495): the "recent sessions" per repo the Repos view reads via rollups.
  // Written in the machine-subtree layout <notes>/<host>/<repo>/<branch>/note.md. These are
  // note-only branches (no live agent / dispatch), so they populate a repo's "Recent" list, its
  // open-PR count, and its todo/blocker badges. The orchestrate run above supplies the "Active"
  // (in-flight) sessions for repo `bn`. ---
  const isoDay = (secs: number) => new Date(secs * 1000).toISOString().slice(0, 10);
  const branchNote = (repo: string, branch: string, front: string): void =>
    write(path.join(notes, HOSTNAME, repo, branch, "note.md"), front);
  branchNote(
    "bn",
    "feature-cost-panel",
    [
      "---",
      "repo: bn",
      "branch: feature-cost-panel",
      `created: ${isoDay(ago(6 * day))}`,
      "status: open",
      "pr: https://github.com/eduuh/bn/pull/460",
      "todos:",
      "  - id: t1",
      "    text: Wire the cost sparkline into the fleet card",
      `    created: ${isoDay(ago(6 * day))}`,
      "    done: false",
      "  - id: t2",
      "    text: Add a per-repo cost column",
      `    created: ${isoDay(ago(5 * day))}`,
      "    done: false",
      "blockers:",
      "  - id: b1",
      "    text: Waiting on the #381 cost core to land",
      `    created: ${isoDay(ago(5 * day))}`,
      "---",
      "",
      "# Goal",
      "",
      "Surface per-repo cost on the dashboard.",
      "",
    ].join("\n"),
  );
  branchNote(
    "bn",
    "docs-readme",
    [
      "---",
      "repo: bn",
      "branch: docs-readme",
      `created: ${isoDay(ago(2 * day))}`,
      "status: open",
      "pr: 455",
      "---",
      "",
      "# Goal",
      "",
      "Refresh the README quick-start.",
      "",
    ].join("\n"),
  );

  // --- Attention list + cron runs: the machine-local event log at <state>/events.jsonl. ---
  // Attention items (`attn` events, folded open by `open_items`) drive /attention + its sidebar nav badge;
  // `cron_run` events drive the last-run health on /cron. Both are flattened `{kind,ts,...}` lines.
  const line = (o: Record<string, unknown>) => JSON.stringify(o);
  const attnEvents = [
    line({ kind: "attn", ts: ago(1800), id: "a1", source: "manual", title: "Prod dashboard is 500ing", severity: "urgent", repo: "bn", branch: "main", link: "https://github.com/eduuh/bn/issues/900" }),
    line({ kind: "attn", ts: ago(3600), id: "a2", source: "pr-queue", title: "PR #481 has merge conflicts", severity: "warn", repo: "bn", branch: "dispatch/web-redesign-500" }),
    line({ kind: "attn", ts: ago(7200), id: "a3", source: "scout-483", title: "Scout: which crate owns the cron registry?", severity: "info", repo: "bn", branch: "dispatch/web-surface-all-483", detail: "Answer: bn-core::crons (crons.toml) + events::cron_status for last-run." }),
  ];
  const cronEvents = [
    // wt-cleanup: healthy, ran recently; refresh-all: last run failed; metrics-snapshot: ok.
    line({ kind: "cron_run", ts: ago(600), job: "wt-cleanup", scope: "global", duration_ms: 1200, exit_code: 0, ok: true }),
    line({ kind: "cron_run", ts: ago(3 * day), job: "wt-cleanup", scope: "global", duration_ms: 1100, exit_code: 0, ok: true }),
    line({ kind: "cron_run", ts: ago(5400), job: "refresh-all", scope: "global", duration_ms: 8400, exit_code: 1, ok: false }),
    line({ kind: "cron_run", ts: ago(1800), job: "metrics-snapshot", scope: "global", duration_ms: 640, exit_code: 0, ok: true }),
  ];
  // The #500 run-attribution that tags the live orchestrator session above as `orchestration` work,
  // so `supervise` stamps its role and the Fleet pins it in the Orchestrators tier.
  const attributionEvents = [
    line({ kind: "run_attribution", ts: ago(300), session_id: orchSid, work_class: "orchestration", repo: "bn", branch: "orchestrator" }),
  ];
  write(path.join(state, "events.jsonl"), [...attnEvents, ...cronEvents, ...attributionEvents].join("\n") + "\n");

  // --- Declared cron manifest: the registry half of /api/cron (joined with the runs above). ---
  write(
    path.join(RUNTIME_DIR, "jobs", "crons.toml"),
    [
      "[[cron]]",
      'job = "wt-cleanup"',
      'on_calendar = "*:0/15"',
      'description = "Prune merged worktrees"',
      "",
      "[[cron]]",
      'job = "refresh-all"',
      'on_calendar = "*-*-* 05:00:00"',
      'description = "Morning refresh of every managed repo"',
      "network = true",
      "",
    ].join("\n"),
  );

  // --- Skills: two global-personal skill files the /skills view lists + edits. ---
  write(
    path.join(RUNTIME_DIR, "skills-personal", "wrap-session.md"),
    "---\ndescription: Wrap up a work session — commit, push, hand off.\nwhen: finishing a work block\nkeywords: [wrap, handoff, commit]\n---\n# Wrap session\n\nSummarize what changed, run the gate, and write a handoff.\n",
  );
  write(
    path.join(RUNTIME_DIR, "skills-personal", "plan-work.md"),
    "---\ndescription: Plan a piece of work before diving in.\nkeywords: [plan, scope]\n---\n# Plan work\n\nBreak the task into todos and record decisions.\n",
  );

  // --- Goals: today's daily file + the current quarter file (aimed-vs-achieved showcase). ---
  const d = new Date();
  const isoDate = (x: Date) => `${x.getFullYear()}-${String(x.getMonth() + 1).padStart(2, "0")}-${String(x.getDate()).padStart(2, "0")}`;
  const today = isoDate(d);
  const qid = `${d.getFullYear()}-Q${Math.floor(d.getMonth() / 3) + 1}`;
  write(
    path.join(notes, HOSTNAME, "daily", `${today}.md`),
    [
      "---",
      `date: ${today}`,
      `created: ${today}`,
      "goals:",
      "  - id: g1",
      "    text: Ship the surface-all dashboard",
      `    created: ${today}`,
      "    done: false",
      "    pct: 60",
      '    est: "90m"',
      '    actual: "50m"',
      "    q: surface-all",
      "    notes:",
      "      - id: n1",
      "        repo: bn",
      "        branch: feature-cost-panel",
      `        created: ${today}`,
      "  - id: g2",
      "    text: Review the plan-review PRs",
      `    created: ${today}`,
      "    done: true",
      `    done_at: ${today}`,
      '    est: "30m"',
      '    actual: "25m"',
      "---",
      "",
      "# Today",
      "",
    ].join("\n"),
  );
  write(
    path.join(notes, HOSTNAME, "quarters", `${qid}.md`),
    [
      "---",
      `quarter: ${qid}`,
      `created: ${today}`,
      "goals:",
      "  - key: surface-all",
      "    text: Make the dashboard the single pane for everything bn",
      `    created: ${today}`,
      "    done: false",
      '    metric: "dashboard_coverage>90%"',
      "  - key: web-redesign",
      "    text: Ship the mobile-first web redesign",
      `    created: ${today}`,
      "    done: true",
      `    done_at: ${today}`,
      "---",
      "",
      `# ${qid}`,
      "",
    ].join("\n"),
  );

  // --- Stub `bn`: success envelope echoing argv, so process writes are deterministic in CI. ---
  const binPath = path.join(RUNTIME_DIR, "bin", "bn");
  write(binPath, STUB_BN);
  fs.chmodSync(binPath, 0o755);
}
