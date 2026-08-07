---
name: dispatch-verify-gate
internal: true
description: Explicit verification requirement for a dispatched ship lane
---
Gate completion on `bn verify`: run the repository build/test/lint gate and report done only after
it passes. Re-run it after any rebase onto an advanced base.
