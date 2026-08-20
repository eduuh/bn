#!/usr/bin/env zsh
set -eu

WORKFLOW_ROOT="${0:A:h:h}"
TMP="$(mktemp -d)"
trap 'rm -rf -- "$TMP"' EXIT

test_base_branch() {
    local base="$1"
    local root="$TMP/$base"
    local source_repo="$root/source"
    mkdir -p "$source_repo" "$root/projects/bare"
    git -C "$source_repo" init -q -b "$base"
    git -C "$source_repo" config user.email test@example.com
    git -C "$source_repo" config user.name Test
    print -r -- "$base" > "$source_repo/seed"
    git -C "$source_repo" add seed
    git -C "$source_repo" commit -qm seed

    git clone -q --bare "$source_repo" "$root/projects/bare/demo.git"
    git --git-dir="$root/projects/bare/demo.git" \
        config remote.origin.fetch '+refs/heads/*:refs/remotes/origin/*'

    source "$WORKFLOW_ROOT/lib/tmux-lib.sh"
    PROJECT_ROOT="$root/projects"
    BARE_DIR="$PROJECT_ROOT/bare"
    WORKTREE_DIR="$PROJECT_ROOT/worktree"

    ensure_worktree demo feature
    [[ "$(git -C "$WORKTREE_DIR/demo/feature" rev-parse HEAD)" ==
       "$(git -C "$source_repo" rev-parse HEAD)" ]]
    ensure_main_worktree demo
    [[ -d "$WORKTREE_DIR/demo/$base" ]]
    [[ "$(bare_repo_main_path demo)" == "$WORKTREE_DIR/demo/$base" ]]
}

test_base_branch main
test_base_branch master
