import { cn } from "../lib/utils";

/** Value tint per tone — the dashboard's ok/urgent/info/warn signal scale, grey → plain text. */
const TONE: Record<string, string> = {
  green: "text-ok",
  red: "text-urgent",
  blue: "text-info",
  amber: "text-warn",
  grey: "text-foreground",
};

/** A single dashboard stat tile (design §4) on the shared Card surface. `tone` colours the value;
 * `title` supplies the hover tooltip that disambiguates the two cost units. */
export function StatTile({
  label,
  value,
  tone = "grey",
  title,
}: {
  label: string;
  value: string;
  tone?: "green" | "red" | "blue" | "amber" | "grey";
  title?: string;
}) {
  return (
    <div
      className="flex flex-1 flex-col gap-1 rounded-lg border border-border bg-card p-4 shadow-card"
      title={title}
    >
      <div className={cn("font-mono text-2xl font-semibold leading-none tabular-nums", TONE[tone])}>
        {value}
      </div>
      <div className="text-xs uppercase tracking-wide text-muted-foreground">{label}</div>
    </div>
  );
}
