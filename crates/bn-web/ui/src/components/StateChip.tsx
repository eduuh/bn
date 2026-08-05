import { paneTone } from "../lib/board";
import type { PaneState } from "../api/types";
import { toneVariant } from "../lib/tone";
import { Badge } from "./ui/badge";

/** A pane-state pill coloured by [`paneTone`] (working green, api/dns/dead red, wip/menus amber,
 * done/idle grey). Renders nothing when the item has no observed state yet. */
export function StateChip({ state }: { state?: PaneState | null }) {
  if (!state) return null;
  return <Badge variant={toneVariant(paneTone(state))}>{state}</Badge>;
}
