import { useCallback, useEffect, useRef, useState } from "react";
import type { FormEvent } from "react";
import type { Terminal } from "@xterm/xterm";
import type { FitAddon } from "@xterm/addon-fit";
import { AlertTriangle, Keyboard, X } from "lucide-react";
import { createTerminal, safeFit } from "../lib/term";
import { pushToast } from "../lib/toast";
import { Button } from "./ui/button";
import { cn } from "../lib/utils";

/** The connection state shown by the status dot + label. */
type Status = "connecting" | "live" | "gone" | "closed";

/** Frames the server pushes over the socket (see `bn_web::terminal`). `frame` is a full pane
 * snapshot (ANSI kept), `gone` means the tmux window vanished, `input-ack`/`denied` answer a keystroke
 * send. */
type ServerMsg =
  | { type: "frame"; lines: string[] }
  | { type: "gone"; reason?: string }
  | { type: "input-ack"; ok: boolean }
  | { type: "denied"; reason?: string };

/** Build the terminal socket URL against the page origin (the bn-web server also serves this SPA).
 * `rows` seeds the initial capture height so the first frame arrives already the right size. */
function socketUrl(target: string, rows: number): string {
  const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
  const u = new URL(
    `${proto}//${window.location.host}/ws/agents/${encodeURIComponent(target)}/terminal`,
  );
  u.searchParams.set("rows", String(Math.max(1, rows)));
  return u.toString();
}

/**
 * The live agent terminal (#491) — an xterm.js view streamed over `GET /ws/agents/:target/terminal`,
 * upgrading the #496 polled tail into a real-time terminal. Frames are full-pane snapshots, so each
 * one resets + rewrites the screen (deduped so an unchanged pane doesn't flicker). "Take control"
 * reveals a composer that forwards a line into the pane (`bn agent send --keys`) — a real
 * input field, so the mobile keyboard has something to focus. The socket reconnects with backoff
 * while the tab is mounted; a `gone` frame (window closed) stops it.
 */
export function AgentTerminal({ target }: { target: string }) {
  const hostRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const socketRef = useRef<WebSocket | null>(null);
  const lastFrameRef = useRef<string>("");
  const retryRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const mountedRef = useRef(true);

  const [status, setStatus] = useState<Status>("connecting");
  const [controlling, setControlling] = useState(false);
  const [input, setInput] = useState("");

  // Create the terminal once. Kept mounted for the component's life; the socket effect writes frames
  // into it. A ResizeObserver re-fits (and reports the new size to the server) as the layout changes.
  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const { term, fit } = createTerminal();
    term.open(host);
    termRef.current = term;
    fitRef.current = fit;
    safeFit(host, fit);

    const ro =
      typeof ResizeObserver !== "undefined"
        ? new ResizeObserver(() => {
            safeFit(host, fit);
            sendResize();
          })
        : null;
    ro?.observe(host);

    return () => {
      ro?.disconnect();
      term.dispose();
      termRef.current = null;
      fitRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  /** Tell the server our current viewport so its captures match (a *soft* resize — the agent's real
   * tmux geometry is never touched). No-op until the socket is open. */
  const sendResize = useCallback(() => {
    const term = termRef.current;
    const ws = socketRef.current;
    if (!term || !ws || ws.readyState !== WebSocket.OPEN) return;
    ws.send(JSON.stringify({ type: "resize", rows: term.rows, cols: term.cols }));
  }, []);

  const writeFrame = useCallback((lines: string[]) => {
    const term = termRef.current;
    const host = hostRef.current;
    const fit = fitRef.current;
    if (!term) return;
    const joined = lines.join("\r\n");
    // Full-frame snapshots repeat while the pane is idle — only redraw on a real change so xterm
    // doesn't flicker/reset every tick.
    if (joined === lastFrameRef.current) return;
    lastFrameRef.current = joined;
    if (host && fit) safeFit(host, fit);
    term.reset();
    term.write(joined);
    term.scrollToBottom();
  }, []);

  // Manage the socket. Reconnects when the target or the paired token changes; retries with a short
  // backoff on an unexpected close, until unmounted or the pane is gone.
  useEffect(() => {
    mountedRef.current = true;
    const connect = () => {
      const term = termRef.current;
      const rows = term?.rows ?? 40;
      let ws: WebSocket;
      try {
        ws = new WebSocket(socketUrl(target, rows));
      } catch {
        setStatus("closed");
        scheduleRetry();
        return;
      }
      socketRef.current = ws;
      setStatus("connecting");

      ws.onopen = () => {
        if (!mountedRef.current) return;
        setStatus("live");
        sendResize();
      };
      ws.onmessage = (ev) => {
        let msg: ServerMsg;
        try {
          msg = JSON.parse(ev.data as string) as ServerMsg;
        } catch {
          return;
        }
        if (msg.type === "frame") {
          setStatus("live");
          writeFrame(msg.lines);
        } else if (msg.type === "gone") {
          setStatus("gone");
        } else if (msg.type === "input-ack") {
          pushToast(msg.ok ? "sent to the agent" : "the pane didn't take the input", msg.ok ? "success" : "error");
        } else if (msg.type === "denied") {
          pushToast(msg.reason ?? "input was rejected", "error");
        }
      };
      ws.onclose = () => {
        if (!mountedRef.current) return;
        // A `gone` pane is terminal — don't fight to reconnect to a window that's closed.
        setStatus((s) => (s === "gone" ? "gone" : "closed"));
        socketRef.current = null;
        scheduleRetry();
      };
      ws.onerror = () => {
        // `onclose` follows and owns the retry; just close so it fires promptly.
        try {
          ws.close();
        } catch {
          /* already closing */
        }
      };
    };

    const scheduleRetry = () => {
      if (!mountedRef.current) return;
      if (retryRef.current) clearTimeout(retryRef.current);
      retryRef.current = setTimeout(() => {
        if (mountedRef.current && status !== "gone") connect();
      }, 1500);
    };

    connect();

    return () => {
      mountedRef.current = false;
      if (retryRef.current) clearTimeout(retryRef.current);
      const ws = socketRef.current;
      socketRef.current = null;
      if (ws) {
        ws.onclose = null;
        ws.onerror = null;
        try {
          ws.close();
        } catch {
          /* already closing */
        }
      }
    };
    // `status` is intentionally excluded — reading it in `scheduleRetry` is a stale-closure-safe
    // "don't retry a gone pane"; the effect must not re-subscribe the socket on every status change.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [target, sendResize, writeFrame]);

  /** Enter control mode — unlock the composer that forwards a line into the pane. */
  const takeControl = () => setControlling(true);

  const submitInput = (e: FormEvent) => {
    e.preventDefault();
    const line = input;
    if (!line.trim()) return;
    const ws = socketRef.current;
    if (!ws || ws.readyState !== WebSocket.OPEN) {
      pushToast("terminal not connected", "error");
      return;
    }
    ws.send(JSON.stringify({ type: "input", data: line }));
    setInput("");
  };

  const statusLabel =
    status === "live"
      ? "live"
      : status === "connecting"
        ? "connecting…"
        : status === "gone"
          ? "window closed"
          : "reconnecting…";

  return (
    <section className="overflow-hidden rounded-lg border border-border bg-card shadow-card">
      <div className="flex flex-wrap items-center gap-2 border-b border-border bg-muted/40 px-3 py-2">
        <h3 className="flex items-center gap-2 text-sm font-semibold text-foreground">
          Terminal
          <span className="truncate font-mono text-xs font-normal text-muted-foreground">
            {target}
          </span>
        </h3>
        <span className="flex-1" />
        {!controlling ? (
          <Button type="button" variant="secondary" size="sm" onClick={takeControl}>
            <Keyboard className="h-4 w-4" /> Take control
          </Button>
        ) : (
          <Button
            type="button"
            variant="ghost"
            size="sm"
            onClick={() => setControlling(false)}
            title="Stop sending keys (the live view keeps streaming)"
          >
            <X className="h-4 w-4" /> Release
          </Button>
        )}
        <span className="inline-flex items-center gap-1.5">
          <span
            className={cn(
              "h-2 w-2 rounded-full",
              status === "live" ? "animate-pulse bg-ok" : "bg-muted-foreground/50",
            )}
            title={statusLabel}
            aria-hidden="true"
          />
          <span data-testid="term-status" className="font-mono text-xs text-muted-foreground">
            {statusLabel}
          </span>
        </span>
      </div>

      {status === "gone" && (
        <div
          data-testid="term-gone"
          className="flex items-center gap-2 border-b border-urgent/30 bg-urgent-muted px-3 py-2 text-xs text-urgent"
        >
          <AlertTriangle className="h-4 w-4 shrink-0" />
          The agent's tmux window is gone — nothing left to stream.
        </div>
      )}

      <div
        className="min-h-[16rem] bg-[#0b0e13] p-2"
        ref={hostRef}
        role="img"
        aria-label={`Live terminal for ${target}`}
      />

      {controlling && (
        <div className="space-y-1.5 border-t border-border bg-muted/40 px-3 py-2.5">
          <form className="flex items-center gap-2" onSubmit={submitInput}>
            <input
              data-testid="term-input"
              className="h-9 flex-1 rounded-md border border-input bg-background px-3 font-mono text-sm text-foreground outline-none ring-offset-background placeholder:text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring"
              value={input}
              onChange={(e) => setInput(e.target.value)}
              placeholder={`Type into ${target} — Enter sends`}
              autoComplete="off"
              autoCapitalize="off"
              autoCorrect="off"
              spellCheck={false}
              aria-label={`Send keys to ${target}`}
            />
            <Button type="submit" size="sm" disabled={!input.trim()}>
              Send
            </Button>
          </form>
          <p className="text-xs text-muted-foreground">
            Keys go straight into the worktree's pane. The live view above reflects them.
          </p>
        </div>
      )}
    </section>
  );
}
