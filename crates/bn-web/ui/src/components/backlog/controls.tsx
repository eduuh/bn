import { Button } from "../ui/button";

/** Small shared form controls for the backlog page. */

/** A clamped −/value/+ integer stepper (concurrency lanes). */
export function Stepper({
  value,
  min = 1,
  max = 32,
  disabled = false,
  onChange,
  label,
}: {
  value: number;
  min?: number;
  max?: number;
  disabled?: boolean;
  onChange: (v: number) => void;
  label?: string;
}) {
  const set = (v: number) => {
    const next = Math.max(min, Math.min(max, v));
    if (next !== value) onChange(next);
  };
  return (
    <div
      className="inline-flex items-center overflow-hidden rounded-md border border-input"
      role="group"
      aria-label={label ?? "stepper"}
    >
      <Button
        type="button"
        variant="ghost"
        size="icon"
        className="rounded-none text-base"
        disabled={disabled || value <= min}
        onClick={() => set(value - 1)}
        aria-label="decrease"
      >
        −
      </Button>
      <span className="min-w-10 text-center font-mono font-bold tabular-nums">{value}</span>
      <Button
        type="button"
        variant="ghost"
        size="icon"
        className="rounded-none text-base"
        disabled={disabled || value >= max}
        onClick={() => set(value + 1)}
        aria-label="increase"
      >
        +
      </Button>
    </div>
  );
}
