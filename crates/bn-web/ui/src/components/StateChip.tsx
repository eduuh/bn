import { paneTone } from "../lib/board";
import type { ChipTone } from "../lib/board";
import type { PaneState } from "../api/types";
import { Badge, type BadgeProps } from "./ui/badge";

/** Map a board [`ChipTone`] to a shadcn Badge variant — the dashboard's shared tone → chip bridge. */
export function toneVariant(tone: ChipTone): BadgeProps["variant"] {
  switch (tone) {
    case "green":
      return "ok";
    case "red":
      return "urgent";
    case "amber":
      return "warn";
    default:
      return "muted";
  }
}

/** A pane-state pill coloured by [`paneTone`] (working green, api/dns/dead red, wip/menus amber,
 * done/idle grey). Renders nothing when the item has no observed state yet. */
export function StateChip({ state }: { state?: PaneState | null }) {
  if (!state) return null;
  return <Badge variant={toneVariant(paneTone(state))}>{state}</Badge>;
}
