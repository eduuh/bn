import { useEffect, useState } from "react";

/** Subscribe to a CSS media query from JS. Used where a layout difference has to change *behaviour*
 * and not just styling — the console auto-selects a row on a wide screen (where the focus pane sits
 * beside the queue) but must not on a phone (where selecting replaces the queue entirely). */
export function useMediaQuery(query: string): boolean {
  const [matches, setMatches] = useState(
    () => typeof window !== "undefined" && window.matchMedia(query).matches,
  );

  useEffect(() => {
    const mql = window.matchMedia(query);
    const onChange = () => setMatches(mql.matches);
    onChange();
    mql.addEventListener("change", onChange);
    return () => mql.removeEventListener("change", onChange);
  }, [query]);

  return matches;
}
