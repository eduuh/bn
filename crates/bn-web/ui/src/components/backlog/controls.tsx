/** Small shared form controls for the backlog page. */

/** A repo combobox: free-text (dispatch/start can target a brand-new repo) with a `datalist` of the
 * repos already seen in the live runs / fleet (`#343` repo select, no repos endpoint needed). */
export function RepoField({
  id,
  value,
  onChange,
  options,
  placeholder = "repo (optional)",
}: {
  id: string;
  value: string;
  onChange: (v: string) => void;
  options: string[];
  placeholder?: string;
}) {
  const listId = `${id}-repos`;
  return (
    <>
      <input
        id={id}
        className="mono"
        list={options.length ? listId : undefined}
        value={value}
        placeholder={placeholder}
        autoComplete="off"
        spellCheck={false}
        onChange={(e) => onChange(e.target.value)}
      />
      {options.length > 0 && (
        <datalist id={listId}>
          {options.map((r) => (
            <option key={r} value={r} />
          ))}
        </datalist>
      )}
    </>
  );
}

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
    <div className="stepper" role="group" aria-label={label ?? "stepper"}>
      <button
        type="button"
        className="step"
        disabled={disabled || value <= min}
        onClick={() => set(value - 1)}
        aria-label="decrease"
      >
        −
      </button>
      <span className="step-value mono">{value}</span>
      <button
        type="button"
        className="step"
        disabled={disabled || value >= max}
        onClick={() => set(value + 1)}
        aria-label="increase"
      >
        +
      </button>
    </div>
  );
}
