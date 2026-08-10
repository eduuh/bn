/** The small uppercase heading each section of the repo page carries, with an optional count chip.
 *
 * Promoted out of `RepoDetail` when the operational controls moved to their own file — two copies of
 * a heading is how two sections of one page start looking subtly different. */
import { Badge } from "./ui/badge";

export function SectionHead({
  icon: Icon,
  title,
  count,
}: {
  icon: React.ComponentType<{ className?: string }>;
  title: string;
  count?: number;
}) {
  return (
    <div className="flex items-center gap-2">
      <Icon className="h-4 w-4 text-muted-foreground" />
      <h3 className="text-sm font-semibold uppercase tracking-wide text-muted-foreground">
        {title}
      </h3>
      {count != null && <Badge variant="muted">{count}</Badge>}
    </div>
  );
}
