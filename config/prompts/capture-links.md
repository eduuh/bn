---
name: capture-links
description: Capture the links from this session worth going back to, before they leave with the worktree
when: A URL surfaced that you will want again — a PR, a work item or PBI, a portal blade, a dashboard, a docs page — or a pull request just completed and its links are about to become unreachable. Also good as a sweep before closing a branch.
keywords: [link, links, url, bookmark, capture, save, pbi, work item, portal, dashboard, docs, reference, come back to]
---
You are capturing the links from this session that are worth going back to.

A link nobody stores is a link somebody re-finds. The branch note holds exactly two of them
(`pr:` and `work_item:`) and both leave when the worktree does, so anything you want next month
belongs in the `bn links` store — person-scoped, cross-machine, and ranked by how often it is
actually opened.

1. **Sweep the session.** Read back over what you and the user touched and list every URL that
   would cost real time to find again: pull requests, work items and PBIs, Azure portal blades,
   dashboards and queries, internal docs and wikis, API references, the one Stack Overflow answer
   that actually worked. Skip anything trivially re-findable — a package's front page, a search
   result, a link the user will never open twice.

2. **Capture the branch's own links first.** If this branch has a PR or a work item, run
   `bn links capture` in the shell — it stores both, named and tagged with the repo, in one
   command. Do this the moment a pull request completes, not at the end of the day: the gap
   between "merged" and "worktree gone" is where these links get lost.

3. **Store the rest**, one call each, with the `bn_links_add` MCP tool (or `bn links add <url>`
   in the shell). Give every link:
   - a `name` you would actually search for — what it *is*, not what its `<title>` says. "Prod
     subscription access request" beats "Microsoft Azure".
   - `tags` from the configured vocabulary (`bn links tags` prints it, with counts). A link takes
     as many as apply. An unlisted tag is kept, not rejected, so reach for a new one when nothing
     fits — but check the set first, because six tags you use beat twenty you don't. Omit `tags`
     and they are guessed from the host: an ICM incident gets `icm`, learn.microsoft.com gets
     `docs`, myaccess gets `permissions`, security.microsoft.com gets `security`.
   - the `repo`, when the link belongs to one. Those sort first when you are in that repo.

4. **Don't fetch anything.** Names are written, never scraped. If you cannot say what a link is
   for, ask the user rather than storing a URL nobody will recognise in three months.

5. **Report** in one line: how many links you stored, and name any you deliberately skipped so
   the user can overrule you.

Be honest about duplicates: re-adding a URL already stored updates it in place, which is the right
move when you are improving a name or fixing tags, and pointless noise otherwise.
