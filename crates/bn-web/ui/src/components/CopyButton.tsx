import { useState } from "react";
import { Check, Copy } from "lucide-react";
import { cn } from "../lib/utils";

/** A one-click "copy to clipboard" button, used for the tmux jump command on each card. Shows a
 * brief `copied ✓` confirmation; the full command is the `title` tooltip. */
export function CopyButton({ text, label }: { text: string; label: string }) {
  const [copied, setCopied] = useState(false);

  const onClick = () => {
    const done = () => {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1200);
    };
    if (navigator.clipboard?.writeText) {
      navigator.clipboard.writeText(text).then(done).catch(() => done());
    } else {
      done();
    }
  };

  return (
    <button
      type="button"
      onClick={onClick}
      title={text}
      className={cn(
        "inline-flex items-center gap-1.5 rounded-md border border-border bg-muted px-2 py-1 font-mono text-xs transition-colors",
        copied ? "border-ok/30 text-ok" : "text-muted-foreground hover:bg-accent hover:text-foreground",
      )}
    >
      {copied ? <Check className="h-3 w-3" /> : <Copy className="h-3 w-3" />}
      {copied ? "copied" : label}
    </button>
  );
}
