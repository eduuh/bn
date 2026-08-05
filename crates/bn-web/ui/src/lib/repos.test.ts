import { describe, expect, it } from "vitest";
import {
  findRepo,
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
      { repo: "bn", managed: true, active_agents: 1, open_prs: 0, session_count: 2, last_activity_ts: 5, aic: null, cost_usd: null, pool: { enabled: false, total: 0, warm: 0, leased: 0, idle: 0, last_warmed_ts: null }, sessions: [] },
    ],
  };

  it("matches case-insensitively", () => {
    expect(findRepo(view, "BN")?.repo).toBe("bn");
    expect(findRepo(view, "nope")).toBeUndefined();
    expect(findRepo(undefined, "bn")).toBeUndefined();
  });
});
