# Releases

bn follows a **strict release** rule:

> A commit reaches `main` only through CI, and every commit that reaches `main` becomes a
> versioned GitHub Release built for every supported platform. No machine ever compiles bn to
> run it.

Everything below exists to make those three sentences true at the same time.

## The chain

```
 PR ──► CI (ci.yml)                         the pull-request gate
  │
  merge
  │
  ▼
main ──► release.yml
          ├─ gate      calls ci.yml on the MERGED commit
          ├─ version   next vX.Y.Z, from tags
          ├─ build     aarch64-apple-darwin  +  x86_64-unknown-linux-gnu
          └─ publish   one release, all platforms, tagged and marked Latest
                              │
                              ▼
                    install.sh downloads it
```

Each arrow is a `needs:` edge, so each is enforced rather than merely intended:

1. **Nothing publishes unless CI is green on the merged commit.** A squash merge rewrites the
   commit, so the SHA a PR proved green is not the SHA that lands on `main`. `release.yml` calls
   `ci.yml` via `workflow_call` rather than copying its jobs, so "passed CI" means the same fmt /
   clippy / test / web / gates / e2e lanes a PR faces — one definition of green, used twice.
2. **Nothing publishes unless every platform built.** A partial release is worse than none: it
   hands one machine a new bn while another silently keeps an old one.
3. **No machine compiles bn to install it.** `install.sh` downloads these assets and nothing
   else. `cargo build` is for developing bn.

## Versioning

**The git tag is the source of truth, never a commit.** `release.yml` computes the next version
and passes it into the build as `BN_RELEASE_VERSION` (see `crates/bn-cli/build.rs`), so CI never
pushes a version-bump commit back to `main`. It cannot: a bump commit on `main` would re-trigger
the release workflow, which would bump again, forever.

`Cargo.toml`'s `[workspace.package] version` is the **floor**:

| you want | do this |
| --- | --- |
| a patch (the default) | nothing — merging to `main` cuts `vX.Y.(Z+1)` |
| a minor or major | raise the floor in an ordinary PR; the next merge cuts `vX.(Y+1).0` |

The version is `max(floor, latest_tag + one patch)`, so an un-bumped `Cargo.toml` simply means
"keep taking patches", and the floor only wins when it is genuinely ahead.

A local build has no `BN_RELEASE_VERSION` and reports the floor, exactly as it always did:

```
$ bn --version
bn 0.5.0 (git de967f24a5ca, built 2026-09-08, aarch64-apple-darwin)   # local build
bn 0.5.1 (git de967f24a5ca, built 2026-09-08, aarch64-apple-darwin)   # released build
```

## Installing and updating

```sh
curl -fsSL https://raw.githubusercontent.com/eduuh/bn/main/install.sh | sh
```

`install.sh` fetches `bn-cli`, `bn-web` and `bn-mcp` for the local target from
`releases/latest` — through an authenticated `gh` first (the repo is private, so an
unauthenticated `curl` on `releases/latest/download` 404s), falling back to `curl`. There is
deliberately **no** compile-from-source fallback behind it: a silent local build is how machines
end up running different bn than the one that passed CI. To build a checkout on purpose — an
unmerged branch, a local patch, a platform the release skips — run `./build.sh`.

Staying current is the `self-update` cron's job: it fast-forwards `bn/main`, and when HEAD has
moved past the last install it runs `install.sh --core`. It asks GitHub what the latest release
was built from (`targetCommitish`) before installing it and refuses to replace the installed
binary with the same or an older commit, so a lagging release can never walk a machine backwards.

## Platforms

| target | built on | why |
| --- | --- | --- |
| `aarch64-apple-darwin` | self-hosted macOS runner | GitHub-hosted macOS bills at 10x on a private repo |
| `x86_64-unknown-linux-gnu` | `ubuntu-22.04` | the oldest glibc we link against is the oldest machine bn installs on — this pin is the support floor, not an accident |

Adding a platform is one entry in `release.yml`'s build matrix plus a target triple in
`scripts/install-toolchain.sh`, which is what `install.sh` uses to pick its asset. Adding a
*binary* needs nothing: the packaging step discovers binaries from `cargo metadata`.

## What this replaced

Two mechanisms both claimed to ship bn, and between them neither did it reliably:

- **`release-plz` + `cargo-dist`** (`release-plz.yml`, `release.yml`, `release-plz.toml`) — a
  bot opened a version-bump PR whose merge pushed a tag that triggered a cargo-dist build. The
  bump-PR half was already disabled: release-plz derives the next version by running
  `cargo package`, and this workspace can never be packaged, because `bn-core`/`bn-cli`
  `include_str!` repo-root files that cannot live inside a crate tarball.
- **A rolling `nightly` tag** (`nightly-release.yml`, `config/jobs/nightly-release.sh`) — each
  host built `main` and uploaded *its own* platform's assets to one shared rolling release. So
  the published release was only ever as fresh as the last machine that happened to be awake. On
  the day it was removed, the darwin assets were two days behind the linux ones, and the darwin
  half came from a cron on a laptop rather than from CI.

Both are gone. There is one channel, one workflow, and one definition of "released".

## Enforcement, honestly

GitHub cannot block the merge itself here. Branch protection and rulesets both return

```
403  Upgrade to GitHub Pro or make this repository public to enable this feature.
```

on a private repository on the free plan. So the enforced gate is **the release**, not the merge:
a red commit can technically land on `main`, but it can never be published, and therefore can
never reach a machine — `install.sh` only ever installs a release, and a release only exists when
CI passed on that exact commit. `main` simply stays un-released until it is fixed, which is loud.

Making the repo public, or paying for Pro, would let GitHub refuse the merge outright. That is
the only thing that would close the remaining gap.
