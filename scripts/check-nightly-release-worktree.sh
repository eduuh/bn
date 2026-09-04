#!/bin/sh
set -eu

repo=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd -P)
helper="$repo/config/jobs/nightly-release-worktree"
scratch="$repo/target/nightly-release-worktree-check"
trap 'rm -rf "$scratch"' EXIT INT TERM
rm -rf "$scratch"
mkdir -p "$scratch/home/.config/bn" "$scratch/worktree/bn"

git init -q --bare "$scratch/remote.git"
git init -q "$scratch/seed"
git -C "$scratch/seed" config user.name "bn test"
git -C "$scratch/seed" config user.email "bn-test@example.invalid"
printf 'one\n' >"$scratch/seed/tracked"
git -C "$scratch/seed" add tracked
git -C "$scratch/seed" commit -qm one
git -C "$scratch/seed" branch -M main
git -C "$scratch/seed" remote add origin "$scratch/remote.git"
git -C "$scratch/seed" push -q -u origin main
git --git-dir="$scratch/remote.git" symbolic-ref HEAD refs/heads/main

git clone -q "$scratch/remote.git" "$scratch/anchor"
git -C "$scratch/anchor" worktree add --detach -q "$scratch/worktree/bn/main" origin/main
rm "$scratch/worktree/bn/main/tracked"
ln -s "$scratch/worktree/bn/main" "$scratch/home/.config/bn/repo"

prepare() (
  HOME="$scratch/home"
  export HOME
  unset BN_MAIN_WORKTREE
  . "$helper"
  bn_nightly_prepare_worktree
)

release=$(prepare)
[ "$release" = "$scratch/worktree/bn/_release-main" ]
[ "$(git -C "$release" rev-parse HEAD)" = "$(git -C "$scratch/seed" rev-parse HEAD)" ]
[ -z "$(git -C "$release" status --porcelain --untracked-files=no)" ]
test ! -e "$scratch/worktree/bn/main/tracked"

printf 'two\n' >>"$scratch/seed/tracked"
git -C "$scratch/seed" commit -qam two
git -C "$scratch/seed" push -q
release=$(prepare)
[ "$(git -C "$release" rev-parse HEAD)" = "$(git -C "$scratch/seed" rev-parse HEAD)" ]
test ! -e "$scratch/worktree/bn/main/tracked"

before=$(git -C "$release" rev-parse HEAD)
printf 'dirty\n' >>"$release/tracked"
if prepare >"$scratch/dirty.out" 2>&1; then
  echo "nightly release check: dirty dedicated checkout was overwritten" >&2
  exit 1
fi
grep -q "refusing to overwrite" "$scratch/dirty.out"
[ "$(git -C "$release" rev-parse HEAD)" = "$before" ]
grep -q dirty "$release/tracked"

override=$(
  HOME="$scratch/home"
  BN_MAIN_WORKTREE="$scratch/worktree/bn/main"
  export HOME BN_MAIN_WORKTREE
  unset GITHUB_ACTIONS
  . "$helper"
  bn_nightly_resolve_worktree
)
[ "$override" = "$scratch/worktree/bn/_release-main" ]

override=$(
  HOME="$scratch/home"
  BN_MAIN_WORKTREE="$scratch/explicit"
  GITHUB_ACTIONS=true
  export HOME BN_MAIN_WORKTREE GITHUB_ACTIONS
  . "$helper"
  bn_nightly_resolve_worktree
)
[ "$override" = "$scratch/explicit" ]

echo "nightly release worktree check: PASS"
