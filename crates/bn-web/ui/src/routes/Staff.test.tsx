import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { Staff } from "./Staff";

afterEach(() => {
  cleanup();
});

describe("Staff", () => {
  it("shows the placeholder instead of probing missing staff APIs", () => {
    render(<Staff />);

    expect(screen.getByRole("heading", { name: "Staff" })).toBeTruthy();
    expect(screen.getByText(/briefing cards are not available on this build yet/i)).toBeTruthy();
    expect(
      screen.getByText(/the server-side staff board\/feed APIs are not implemented/i),
    ).toBeTruthy();
  });
});
