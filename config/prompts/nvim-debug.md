---
name: nvim-debug
description: "Investigate runtime failures with Neovim DAP, stack frames, locals, watches and the debug console. Use when existing code needs breakpoint-driven diagnosis or static inspection cannot establish the cause."
when: "Debugging complex existing code, an async failure, incorrect runtime values, or behavior that must be understood before changing code."
keywords: [nvim debug, neovim debugger, dap, breakpoint, conditional breakpoint, logpoint, debug console, inspect locals, stack frame, async failure, attach debugger, runtime state]
arguments:
  - { name: symptom, description: The observed failure and the program or test to reproduce it., required: false }
---
Use the existing `debug` workflow for the fix lifecycle; this skill supplies runtime evidence,
not a second implementation pipeline. Recipes and verified adapter behavior are in
`config/reference/nvim-explore.md` in bn's source checkout.

1. **Reproduce.** Read the branch context and relevant repo references. Pin the symptom
   ({{symptom}}), expected result, input and exact failing command before editing code.
   If it does not reproduce, record the missing condition instead of guessing a cause.
2. **Preflight.** Use the worktree's editor and nvim MCP discovery; inspect its active DAP
   session before acting. Do not replace someone's session or unsaved buffers. Verify the
   configured adapter and runtime exist; use the existing dependency installer only when
   needed and authorized. LSP readiness is not debugger readiness.
3. **Choose the target.** Prefer the repo's launch/test configuration. For attach, verify the
   process, worktree, inspector address and source mapping; keep listeners on loopback.
   Record whether the process is yours. Never restart or signal a shared service implicitly.
4. **Stop narrowly.** Put a conditional breakpoint or logpoint on the smallest suspect path.
   Start with uncaught exceptions; enable caught exceptions only for the hypothesis being
   tested. Wait for the intended thread, source and current frame, not just a stopped event.
5. **Observe.** Request stack frames, scopes and locals; compare a watch before and after
   stepping. Test the actual debug console, using adapter-specific syntax. Expressions can
   mutate state: do not call application functions or inspect secrets without justification.
   Record the repro, source location and relevant values; use bounded waits and explicit errors.
6. **Prove the cause.** Follow the data into its caller or async stack and rule out competing
   explanations. Hand the observed cause back to `debug`; keep the fix surgical, add a
   regression test and rerun the original repro plus the repository's relevant checks.
7. **Clean up.** Remove only your breakpoints and listeners, restore exception settings,
   and detach from attached processes with `terminateDebuggee = false`. Terminate only your
   disposable launched processes. Preserve the user's editor, save redacted evidence in bn,
   and report tested behavior separately from unverified configurations or missing tools.
