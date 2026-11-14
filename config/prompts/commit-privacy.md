---
name: commit-privacy
description: "Keep the user's private email out of Git commits by requiring their GitHub noreply address. Use before creating commits, merging, rebasing, cherry-picking, or publishing the user's commits."
when: Creating or publishing commits for the user, including operations that create commits indirectly.
keywords:
  [
    commit,
    git,
    email,
    privacy,
    noreply,
    author,
    committer,
    merge,
    rebase,
    cherry-pick,
    push,
  ]
---

Use `31909722+eduuh@users.noreply.github.com` for both author and committer email on
every new commit authored by the user. This is an agent workflow, not a Git hook:
it cannot block commits made outside the workflow.

1. Before committing, check the effective author with `git var GIT_AUTHOR_IDENT`
   and committer with `git var GIT_COMMITTER_IDENT` under the same environment
   and Git options as the write command. Compare emails without printing other
   addresses. A configured `user.email` alone is not proof: environment variables
   and `--author` can override it.
2. Set both emails on each write command; do not rely on inherited configuration
   or change global/shared repository settings. For example, with the intended
   commit arguments appended:
   ```sh
   GIT_AUTHOR_EMAIL=31909722+eduuh@users.noreply.github.com \
   GIT_COMMITTER_EMAIL=31909722+eduuh@users.noreply.github.com \
   git -c user.email=31909722+eduuh@users.noreply.github.com commit
   ```
   Reject conflicting `--author` values. Keep names and required coauthor trailers;
   never copy the user's other addresses into messages, trailers, notes, or logs.
3. Before publishing, verify the stored author and committer emails of every
   commit just created using `%ae` and `%ce`, not mailmap substitutions `%aE` and
   `%cE`. Compare silently against the approved address. On mismatch, stop: do not
   push, claim success, amend, or rewrite history without permission.
4. Rebases and cherry-picks can preserve old authors despite environment settings.
   Preserve other people's authorship; do not relabel their commits as the user's.
   If the user's existing commits contain another address, stop before publishing
   and ask permission for a scoped history repair. Never force-push automatically.
