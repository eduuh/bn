import type { ReactNode } from "react";

import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "./ui/alert-dialog";
import { Button, type ButtonProps } from "./ui/button";
import { buttonVariants } from "./ui/button-variants";
import { cn } from "../lib/utils";

/**
 * A button that asks before it fires.
 *
 * This replaces `window.confirm`, which was wrong here in three ways. It blocks the whole tab while
 * the dashboard's SSE stream keeps pushing; it can only render one flat string, so the command
 * about to run cannot be shown as code; and browsers let a user tick "prevent this page from
 * creating more dialogs", which silently turns every later confirm into an instant *cancel* — a
 * destructive action that quietly stops working is worse than one that never had a guard.
 *
 * The trigger is the button itself, so the caller writes the button it wants (`variant`, `size`,
 * icon, label) and gets the confirmation for free.
 */
export function ConfirmButton({
  title,
  description,
  confirmLabel = "Confirm",
  cancelLabel = "Cancel",
  destructive,
  onConfirm,
  children,
  className,
  ...button
}: {
  title: ReactNode;
  description?: ReactNode;
  confirmLabel?: string;
  cancelLabel?: string;
  /** Renders the confirm in the destructive tone — and, deliberately, *not* as the safe default. */
  destructive?: boolean;
  onConfirm: () => void;
  children: ReactNode;
} & Omit<ButtonProps, "onClick" | "title" | "children">) {
  return (
    <AlertDialog>
      <AlertDialogTrigger asChild>
        <Button className={className} {...button}>
          {children}
        </Button>
      </AlertDialogTrigger>
      <AlertDialogContent className="max-w-sm">
        <AlertDialogHeader>
          <AlertDialogTitle>{title}</AlertDialogTitle>
          {description && <AlertDialogDescription>{description}</AlertDialogDescription>}
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>{cancelLabel}</AlertDialogCancel>
          {destructive ? (
            // Not `AlertDialogAction`: that renders the primary button, and a destructive verb
            // should never look like the safe default sitting under the Enter key.
            <AlertDialogCancel
              className={cn(buttonVariants({ variant: "destructive", size: "sm" }))}
              onClick={onConfirm}
            >
              {confirmLabel}
            </AlertDialogCancel>
          ) : (
            <AlertDialogAction onClick={onConfirm}>{confirmLabel}</AlertDialogAction>
          )}
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
