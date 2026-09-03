---
name: get-context
description: Load branch note and investigation files for the current git branch
when: Load branch note and investigation files for the current git branch.
keywords: [get-context, get, context, load, branch, note, investigation, files, current, git]
---
## Branch Note
!`bn --cat 2>/dev/null || echo "No branch note found. Run bn to create one."`

## Investigation Files
!`bn-investigation-files.sh`

Use the above context to understand what the user is working on, their goals, open todos, and any prior investigation.

As you work, update the branch note:
- `bn add todo "text"` / `bn add blocker "text"` / `bn add decision "text"` / `bn add research "text"`
- Save longer artifacts to: `$(bn --path)/filename.md`
