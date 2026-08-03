import { useState } from "react";
import { Button } from "./ui/button";
import { cn } from "../lib/utils";

/**
 * A button that opens a one-line steer box before it fires (#1006).
 *
 * The steer is the operator's "things to look out for", and it goes into the agent's seed
 * verbatim. Making it an inline reveal rather than a modal matters: steering is the common case,
 * not the exception, so it should cost one click and stay on the row you were already reading.
 *
 * Submitting with an empty box is allowed — an unsteered run is a legitimate choice, and forcing
 * a sentence would just teach people to type "n/a".
 */
export function SteerButton({
  label,
  title,
  placeholder,
  busy,
  onSubmit,
}: {
  label: string;
  title?: string;
  placeholder?: string;
  busy?: boolean;
  onSubmit: (steer: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [steer, setSteer] = useState("");

  if (!open) {
    return (
      <Button
        size="sm"
        variant="secondary"
        title={title}
        disabled={busy}
        onClick={() => setOpen(true)}
      >
        {label}
      </Button>
    );
  }

  const fire = () => {
    onSubmit(steer.trim());
    setSteer("");
    setOpen(false);
  };

  return (
    <span className="flex min-w-0 flex-1 basis-full items-center gap-1.5">
      <input
        autoFocus
        value={steer}
        placeholder={placeholder}
        onChange={(e) => setSteer(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") fire();
          if (e.key === "Escape") setOpen(false);
        }}
        aria-label={`${label} — things to look out for`}
        className={cn(
          "min-w-0 flex-1 rounded-[7px] border border-border bg-background px-2 py-1",
          "text-[12px] text-foreground placeholder:text-muted-foreground/70",
          "focus:border-primary focus:outline-none",
        )}
      />
      <Button size="sm" disabled={busy} onClick={fire}>
        {label}
      </Button>
      <Button size="sm" variant="secondary" onClick={() => setOpen(false)}>
        Cancel
      </Button>
    </span>
  );
}
