import type { ChipTone } from "./board";
import type { BadgeProps } from "../components/ui/badge";

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
