// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import type { ReactNode } from "react";
import { Skills } from "./Skills";
import * as writes from "../api/writes";

/** Envelope helper — reads unwrap `{ ok, data }`. */
const env = (command: string, data: unknown) => ({
  ok: true,
  json: async () => ({ ok: true, command, data }),
});

const SKILLS = {
  count: 1,
  skills: [
    {
      name: "wrap-session",
      description: "Wrap up the session",
      scope: "global-personal",
      path: "/skills/wrap-session.md",
      effective: true,
    },
  ],
};

const DETAIL = {
  skill: SKILLS.skills[0],
  content: "---\ndescription: Wrap up the session\n---\nbody\n",
};

/** What every mutation returns: the affected skill + the Claude/Copilot native sync summary. */
const MUTATION = {
  skill: SKILLS.skills[0],
  native: {
    synced: true,
    harnesses: ["claude", "copilot"],
    created: 1,
    updated: 0,
    removed: 0,
    unchanged: 4,
    failures: [],
  },
};

function wrap(node: ReactNode) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter>{node}</MemoryRouter>
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string | URL) => {
      const u = String(url);
      if (u.startsWith("/api/skills/")) return env("skill", DETAIL) as unknown as Response;
      if (u.startsWith("/api/skills")) return env("skills", SKILLS) as unknown as Response;
      throw new Error(`unexpected fetch: ${u}`);
    }),
  );
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("Skills — create (#483)", () => {
  it("creates a skill in the chosen tier via POST /api/skills", async () => {
    const spy = vi.spyOn(writes, "createSkill").mockResolvedValue(MUTATION);
    wrap(<Skills />);

    fireEvent.click(await screen.findByRole("button", { name: /New skill/ }));
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "ship-it" } });
    fireEvent.change(screen.getByLabelText("Description"), { target: { value: "Ship a branch" } });
    fireEvent.click(screen.getByRole("button", { name: /Create skill/ }));

    await waitFor(() =>
      expect(spy).toHaveBeenCalledWith({
        name: "ship-it",
        description: "Ship a branch",
        // The default tier is the private, always-available one.
        scope: "global-personal",
        body: "",
      }),
    );
  });

  it("keeps create disabled until the name charset and description are both valid", async () => {
    wrap(<Skills />);
    fireEvent.click(await screen.findByRole("button", { name: /New skill/ }));
    const create = () => screen.getByRole("button", { name: /Create skill/ }) as HTMLButtonElement;

    expect(create().disabled).toBe(true);
    // A name outside the file-stem charset is rejected client-side too (the server also 400s).
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "bad.name" } });
    fireEvent.change(screen.getByLabelText("Description"), { target: { value: "d" } });
    expect(create().disabled).toBe(true);

    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "good-name" } });
    expect(create().disabled).toBe(false);
  });
});

describe("Skills — delete (#483)", () => {
  it("requires a confirm click before DELETE /api/skills/:name", async () => {
    const spy = vi.spyOn(writes, "deleteSkill").mockResolvedValue({
      ...MUTATION,
      remaining_scope: null,
    });
    wrap(<Skills />);

    fireEvent.click(await screen.findByRole("button", { name: /^wrap-session/ }));
    await screen.findByLabelText("Edit wrap-session skill");

    // First click only arms the delete — nothing is sent.
    fireEvent.click(screen.getByRole("button", { name: /^Delete$/ }));
    expect(spy).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: /Confirm delete/ }));
    await waitFor(() => expect(spy).toHaveBeenCalledWith("wrap-session"));
  });

  it("saves an edit through PUT /api/skills/:name only once the draft is dirty", async () => {
    const spy = vi.spyOn(writes, "saveSkill").mockResolvedValue(MUTATION);
    wrap(<Skills />);

    fireEvent.click(await screen.findByRole("button", { name: /^wrap-session/ }));
    const editor = await screen.findByLabelText("Edit wrap-session skill");
    const save = () => screen.getByRole("button", { name: /Save skill/ }) as HTMLButtonElement;

    await waitFor(() => expect(save().disabled).toBe(true));
    fireEvent.change(editor, { target: { value: "---\ndescription: d\n---\nnew body\n" } });
    fireEvent.click(save());
    await waitFor(() =>
      expect(spy).toHaveBeenCalledWith("wrap-session", "---\ndescription: d\n---\nnew body\n"),
    );
  });
});
