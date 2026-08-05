import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Check, ChevronsUpDown, FolderGit2 } from "lucide-react";
import { reposQuery } from "../api/client";
import { Button } from "./ui/button";
import { Command, CommandEmpty, CommandInput, CommandItem, CommandList } from "./ui/command";
import { Dialog, DialogContent, DialogTitle } from "./ui/dialog";
import { cn } from "../lib/utils";

/**
 * The one repo picker.
 *
 * Every form that starts work needs a repo, and each one used to be a free-text `<input>` with a
 * `<datalist>` built from whatever happened to be in the current fleet or daemon runs. Two problems
 * with that: the options were a *subset* of the repos bn can actually work in (a managed repo with
 * no live agent never appeared), and a typo went straight through to `bn dispatch`, which then
 * failed somewhere far from where the mistake was made.
 *
 * So: one component, one option source — `GET /api/repos`'s `known`, the union of managed repos,
 * local bare clones, and anything with a branch note — and no free text. Searchable because the
 * list is not short, and a dropdown you have to scroll is worse than the input it replaced.
 *
 * Built on `cmdk` + `Dialog` (the same construction as the command palette) rather than a popover:
 * both are already dependencies, and a modal list behaves correctly inside the dialogs half these
 * forms live in, where a nested popover fights the focus trap.
 */
export function RepoSelect({
  value,
  onChange,
  id,
  required,
  disabled,
  className,
  allowClear,
  placeholder = "pick a repo",
}: {
  value: string;
  onChange: (repo: string) => void;
  id?: string;
  /** Only affects the empty-state label — the caller still owns whether it can submit. */
  required?: boolean;
  disabled?: boolean;
  className?: string;
  /** Offer an explicit "any repo" entry that clears the selection. For *filters*, where no repo
   * means "all of them" — never for a field that starts work, where empty means "unanswered". */
  allowClear?: boolean;
  placeholder?: string;
}) {
  const [open, setOpen] = useState(false);
  const repos = useQuery(reposQuery());
  const known = repos.data?.known ?? [];

  // A repo the server no longer lists (renamed, removed) still shows as the current value rather
  // than silently reading as empty — a form that quietly forgets what you picked is worse than one
  // showing something stale you can see and change.
  const options = value && !known.includes(value) ? [value, ...known] : known;

  return (
    <>
      <Button
        id={id}
        type="button"
        variant="secondary"
        disabled={disabled}
        aria-haspopup="listbox"
        aria-expanded={open}
        className={cn("w-full justify-between font-mono font-normal", className)}
        onClick={() => setOpen(true)}
      >
        <span className={cn("truncate", !value && "text-muted-foreground")}>
          {value || (required ? `${placeholder} (required)` : placeholder)}
        </span>
        <ChevronsUpDown className="h-4 w-4 shrink-0 opacity-50" aria-hidden="true" />
      </Button>

      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent
          hideClose
          className="overflow-hidden p-0 sm:max-w-md"
          aria-label="Pick a repo"
        >
          <DialogTitle className="sr-only">Pick a repo</DialogTitle>
          <Command>
            <CommandInput placeholder="Search repos…" />
            <CommandList className="max-h-[min(60vh,22rem)]">
              <CommandEmpty>
                {repos.isLoading
                  ? "Loading repos…"
                  : repos.isError
                    ? "Could not load repos."
                    : known.length === 0
                      ? "bn knows no repos yet — clone one or add it to [managed_repos]."
                      : "No repo matches."}
              </CommandEmpty>
              {allowClear && (
                <CommandItem
                  value="__any__"
                  onSelect={() => {
                    onChange("");
                    setOpen(false);
                  }}
                  className="gap-2"
                >
                  <span className="min-w-0 flex-1 truncate text-muted-foreground">any repo</span>
                  {!value && <Check className="h-4 w-4 shrink-0 text-ok" />}
                </CommandItem>
              )}
              {options.map((repo) => (
                <CommandItem
                  key={repo}
                  value={repo}
                  onSelect={() => {
                    onChange(repo);
                    setOpen(false);
                  }}
                  className="gap-2 font-mono"
                >
                  <FolderGit2
                    className="h-4 w-4 shrink-0 text-muted-foreground"
                    aria-hidden="true"
                  />
                  <span className="min-w-0 flex-1 truncate">{repo}</span>
                  {repo === value && <Check className="h-4 w-4 shrink-0 text-ok" />}
                </CommandItem>
              ))}
            </CommandList>
          </Command>
        </DialogContent>
      </Dialog>
    </>
  );
}
