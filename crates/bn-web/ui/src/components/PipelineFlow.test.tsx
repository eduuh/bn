// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { PipelineFlow } from "./PipelineFlow";
import type { PipelineStage } from "../api/types";

afterEach(cleanup);

const STAGES: PipelineStage[] = [
  // `pickup` runs no workflow skill \u2014 a real `null`, not a gap in the fixture.
  { id: "pickup", glyph: "\u25e6", position: 1, count: 2, skill: null },
  { id: "investigate", glyph: "\ud83d\udd0d", position: 2, count: 0, skill: "investigate" },
  { id: "plan", glyph: "\u25c7", position: 3, count: 1, skill: "plan-investigate" },
];

describe("PipelineFlow", () => {
  it("draws every stage, including the empty ones", () => {
    // An empty stage is a *fact* about the pipeline. Hiding it would make the flow read as
    // shorter than it is, and "nothing is in investigate" is exactly what you want to see.
    render(<PipelineFlow stages={STAGES} />);
    expect(screen.getByLabelText("pickup: 2 in flight")).toBeTruthy();
    expect(screen.getByLabelText("investigate: 0 in flight")).toBeTruthy();
    expect(screen.getByLabelText("plan: 1 in flight")).toBeTruthy();
  });

  it("totals what is in flight", () => {
    render(<PipelineFlow stages={STAGES} />);
    expect(screen.getByText("3 in flight")).toBeTruthy();
  });

  it("says so plainly when the pipeline is empty", () => {
    render(<PipelineFlow stages={STAGES.map((s) => ({ ...s, count: 0 }))} />);
    expect(screen.getByText("nothing in flight")).toBeTruthy();
  });

  it("is a control, not decoration: clicking selects a stage", () => {
    const onSelect = vi.fn();
    render(<PipelineFlow stages={STAGES} onSelect={onSelect} />);
    fireEvent.click(screen.getByLabelText("pickup: 2 in flight"));
    expect(onSelect).toHaveBeenCalledWith("pickup");
  });

  it("clicking the selected stage clears the filter rather than re-applying it", () => {
    const onSelect = vi.fn();
    render(<PipelineFlow stages={STAGES} selected="pickup" onSelect={onSelect} />);
    fireEvent.click(screen.getByLabelText("pickup: 2 in flight"));
    expect(onSelect).toHaveBeenCalledWith(undefined);
  });

  it("renders nothing when the board has no stages", () => {
    const { container } = render(<PipelineFlow stages={[]} />);
    expect(container.innerHTML).toBe("");
  });

  it("captions each stage with the skill it dispatches, and nothing where there is none", () => {
    // The drawing should say what a stage *does*, not only how many sit in it. `pickup` runs no
    // workflow, so it gets no caption rather than a plausible-looking placeholder.
    render(<PipelineFlow stages={STAGES} />);
    expect(screen.getByText("plan-investigate")).toBeTruthy();
    expect(screen.getByTitle("runs the `investigate` skill")).toBeTruthy();
    const pickup = screen.getByLabelText("pickup: 2 in flight");
    expect(pickup.querySelector("[title]")).toBeNull();
  });
});
