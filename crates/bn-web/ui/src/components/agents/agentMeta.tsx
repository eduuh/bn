import type { AgentDefRow } from "../../api/types";
import { Badge } from "../ui/badge";

/**
 * How the roster describes an agent in passing — shared by the sidebar row and the editor header so
 * the same agent never reads as two different things on one screen.
 *
 * Its own module rather than exports hanging off a component file: fast refresh only tracks files
 * that export components exclusively, and a row that re-renders stale after every edit is a worse
 * trade than one extra file.
 */

/** When this agent runs, in the fewest words that are still true. */
export function trigger(a: AgentDefRow): string {
  if (a.specialist) return "installed";
  if (a.schedule) return a.schedule;
  if (a.startup) return "startup";
  return "on demand";
}

/** The chip that says what *kind* of thing this is — the one distinction that changes how you read
 * everything else on the row. A specialist never runs; a staff agent produces briefing cards. */
export function kindBadge(a: Pick<AgentDefRow, "kind" | "specialist">) {
  if (a.specialist) return <Badge variant="muted">specialist</Badge>;
  if (a.kind === "staff") return <Badge variant="info">staff</Badge>;
  if (a.kind === "hosted") return <Badge variant="warn">hosted</Badge>;
  return <Badge variant="outline">command</Badge>;
}
