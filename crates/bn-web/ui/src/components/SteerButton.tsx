import { useState } from "react";
import type { LucideIcon } from "lucide-react";

import { Button } from "./ui/button";
import { Label } from "./ui/label";
import { Popover, PopoverContent, PopoverTrigger } from "./ui/popover";
import { Textarea } from "./ui/textarea";

/**
 * A button that opens a one-line steer box before it fires (#1006).
 *
 * The steer is the operator's "things to look out for", and it goes into the agent's seed
 * verbatim. It is anchored to the button in a `Popover` rather than opening a modal: steering is
 * the common case, not the exception, so it should cost one click and never take you off the row
 * you were already reading. (It used to reveal an input *inside* the row, which reflowed the card
 * and pushed every sibling down — the popover floats instead.)
 *
 * Submitting with an empty box is allowed — an unsteered run is a legitimate choice, and forcing a
 * sentence would just teach people to type "n/a".
 */
export function SteerButton({
  label,
  title,
  placeholder,
  busy,
  icon: Icon,
  variant = "secondary",
  onSubmit,
}: {
  label: string;
  title?: string;
  placeholder?: string;
  busy?: boolean;
  icon?: LucideIcon;
  variant?: "default" | "secondary" | "outline" | "ghost";
  onSubmit: (steer: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [steer, setSteer] = useState("");

  const fire = () => {
    onSubmit(steer.trim());
    setSteer("");
    setOpen(false);
  };

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button size="sm" variant={variant} title={title} disabled={busy}>
          {Icon && <Icon />}
          {label}
        </Button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-80 space-y-2">
        <Label htmlFor="steer" className="text-[11px]">
          {label} — things to look out for
        </Label>
        <Textarea
          id="steer"
          autoFocus
          rows={3}
          value={steer}
          placeholder={placeholder}
          onChange={(e) => setSteer(e.target.value)}
          // Enter submits because the steer is usually one sentence; Shift+Enter still breaks a
          // line for the times it isn't. Escape is handled by the popover itself.
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              fire();
            }
          }}
          className="min-h-[64px] text-[12px]"
        />
        <div className="flex justify-end gap-2">
          <Button size="sm" variant="ghost" onClick={() => setOpen(false)}>
            Cancel
          </Button>
          <Button size="sm" disabled={busy} onClick={fire}>
            {label}
          </Button>
        </div>
        <p className="text-[10.5px] leading-snug text-muted-foreground">
          Leave it empty to run unsteered.
        </p>
      </PopoverContent>
    </Popover>
  );
}
