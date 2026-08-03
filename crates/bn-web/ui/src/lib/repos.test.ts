import { describe, expect, it } from "vitest";
import {
  findRepo,
  forgeLinks,
  sessionStatusLabel,
  sessionTo,
  sessionTone,
  splitSessions,
} from "./repos";
import type { RepoSession, ReposView } from "../api/types";

function session(over: Partial<RepoSession>): RepoSession {
  return {
    repo: "bn",
    branch: "main",
    status: null,
    pr: null,
    open_todos: 0,
    blockers: 0,
    worktree: null,
    path: null,
    last_activity_ts: 0,
    session_id: null,
    health: null,
    target: null,
    output_tokens: 0,
    in_flight: false,
    ...over,
  };
}

describe("sessionTone", () => {
  it("uses live health when the session is running", () => {
    expect(sessionTone(session({ health: "active", in_flight: true }))).toBe("green");
    expect(sessionTone(session({ health: "stuck", in_flight: true }))).toBe("red");
    expect(sessionTone(session({ health: "idle", in_flight: true }))).toBe("amber");
  });

  it("is green for an in-flight dispatch with no joined agent, grey for a note-only branch", () => {
    expect(sessionTone(session({ in_flight: true }))).toBe("green");
    expect(sessionTone(session({ in_flight: false }))).toBe("grey");
  });
});

describe("sessionStatusLabel", () => {
  it("prefers health, then in-flight state, then note status", () => {
    expect(sessionStatusLabel(session({ health: "active", in_flight: true }))).toBe("active");
    expect(sessionStatusLabel(session({ in_flight: true, status: "working" }))).toBe("working");
    expect(sessionStatusLabel(session({ in_flight: true }))).toBe("in-flight");
    expect(sessionStatusLabel(session({ status: "open" }))).toBe("open");
    expect(sessionStatusLabel(session({}))).toBe("note");
  });
});

describe("sessionTo", () => {
  it("links to the agent detail only when a live session backs the branch", () => {
    expect(sessionTo(session({ session_id: "sess-1" }))).toBe("/agents/sess-1");
    expect(sessionTo(session({}))).toBeNull();
  });

  it("encodes the session id for the URL", () => {
    expect(sessionTo(session({ session_id: "a/b c" }))).toBe("/agents/a%2Fb%20c");
  });
});

describe("splitSessions", () => {
  it("separates in-flight (active) from note-only (recent), preserving order", () => {
    const sessions = [
      session({ branch: "a", in_flight: true }),
      session({ branch: "b", in_flight: false }),
      session({ branch: "c", in_flight: true }),
    ];
    const { active, recent } = splitSessions(sessions);
    expect(active.map((s) => s.branch)).toEqual(["a", "c"]);
    expect(recent.map((s) => s.branch)).toEqual(["b"]);
  });
});

describe("findRepo", () => {
  const view: ReposView = {
    repos: [
      { repo: "bn", managed: true, active_agents: 1, open_prs: 0, session_count: 2, last_activity_ts: 5, aic: null, cost_usd: null, pool: { enabled: false, total: 0, warm: 0, leased: 0, idle: 0, last_warmed_ts: null, slots: [] }, links: null, sessions: [] },
    ],
    known: ["bn"],
  };

  it("matches case-insensitively", () => {
    expect(findRepo(view, "BN")?.repo).toBe("bn");
    expect(findRepo(view, "nope")).toBeUndefined();
    expect(findRepo(undefined, "bn")).toBeUndefined();
  });
});

describe("forgeLinks", () => {
  it("names each page the way its own forge does", () => {
    expect(
      forgeLinks({
        forge: "github",
        repo: "https://github.com/eduuh/bn",
        issues: "https://github.com/eduuh/bn/issues",
        pulls: "https://github.com/eduuh/bn/pulls",
      }).map((l) => l.label),
    ).toEqual(["GitHub", "issues", "pulls"]);

    // Azure DevOps has work items and pull requests, not issues and pulls.
    expect(
      forgeLinks({
        forge: "azure-devops",
        repo: "https://dev.azure.com/contoso/Payments/_git/api",
        issues: "https://dev.azure.com/contoso/Payments/_workitems",
        pulls: "https://dev.azure.com/contoso/Payments/_git/api/pullrequests",
      }).map((l) => l.label),
    ).toEqual(["Azure DevOps", "work items", "PRs"]);

    expect(
      forgeLinks({
        forge: "gitlab",
        repo: "https://gitlab.com/g/p",
        issues: "https://gitlab.com/g/p/-/issues",
        pulls: "https://gitlab.com/g/p/-/merge_requests",
      }).map((l) => l.label),
    ).toEqual(["GitLab", "issues", "merge requests"]);
  });

  it("renders only the links the server resolved", () => {
    // An unknown forge has a code page and nothing else — never a guessed issues path.
    const unknown = forgeLinks({
      forge: "unknown",
      repo: "https://git.example.com/team/thing",
      issues: null,
      pulls: null,
    });
    expect(unknown).toEqual([{ href: "https://git.example.com/team/thing", label: "remote" }]);
    // No clone here, or an origin that didn't parse: nothing to link at all.
    expect(forgeLinks(null)).toEqual([]);
    expect(forgeLinks(undefined)).toEqual([]);
  });
});
