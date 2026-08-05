import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

/** shadcn's class combiner: merge conditional `clsx` classes, de-duplicating conflicting Tailwind
 * utilities via `tailwind-merge` (so a later `px-4` wins over an earlier `px-2`). */
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}
