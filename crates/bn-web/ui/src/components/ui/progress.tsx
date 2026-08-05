import * as React from "react";
import * as ProgressPrimitive from "@radix-ui/react-progress";

import { cn } from "@/lib/utils";

/**
 * A determinate progress bar on the dashboard's signal scale.
 *
 * `tone` rather than a colour prop: these bars stand for goal completion and quarter health, which
 * already have meanings in the ok/warn/urgent/info palette the badges use, and letting a caller
 * pass an arbitrary colour is how two screens end up disagreeing about what green means.
 */
const Progress = React.forwardRef<
  React.ElementRef<typeof ProgressPrimitive.Root>,
  React.ComponentPropsWithoutRef<typeof ProgressPrimitive.Root> & {
    tone?: "primary" | "ok" | "warn" | "urgent" | "info";
  }
>(({ className, value, tone = "primary", ...props }, ref) => (
  <ProgressPrimitive.Root
    ref={ref}
    className={cn("relative h-1.5 w-full overflow-hidden rounded-full bg-muted", className)}
    {...props}
  >
    <ProgressPrimitive.Indicator
      className={cn(
        "h-full w-full flex-1 transition-transform duration-500",
        tone === "ok" && "bg-ok",
        tone === "warn" && "bg-warn",
        tone === "urgent" && "bg-urgent",
        tone === "info" && "bg-info",
        tone === "primary" && "bg-primary",
      )}
      style={{ transform: `translateX(-${100 - (value ?? 0)}%)` }}
    />
  </ProgressPrimitive.Root>
));
Progress.displayName = ProgressPrimitive.Root.displayName;

export { Progress };
