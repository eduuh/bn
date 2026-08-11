#!/usr/bin/env bash
# explore-wira360 — the wira360 lane of the explore pass. Gated.
#
# The per-repo settings live HERE rather than on the agent manifest's `run:` line: `bn run <job>` is
# captured by the queue and re-dispatched later in a fresh process that inherits none of the
# environment the original command line set. Settings inside the script survive that; settings on
# the command line silently do not.
#
# Two gates, because wira360 is product-facing:
#   * LOCAL stack only — `dev:forge:family` pins its own ports and a local seeded postgres. A bug
#     "reproduced" against a deployed environment is not a reproduction, and the writes an explorer
#     performs while poking at forms would be destructive there.
#   * `needs-triage` — findings are held out of the auto-pull by `[orchestrate] exclude_labels`
#     until a human drops the label. Discovery is autonomous here; execution is not.
set -u

BN_EXPLORE_REPO=wira360
BN_EXPLORE_MAX="${BN_EXPLORE_MAX:-5}"
BN_EXPLORE_LABEL="${BN_EXPLORE_LABEL:-needs-triage}"

if [ -z "${BN_EXPLORE_FOCUS:-}" ]; then
  BN_EXPLORE_FOCUS="the forge product. Boot the LOCAL dev stack only: bn build run handles it -- docker postgres on 55432, api-host on 55050, forge-web on 15621, seeded. Never a deployed or shared environment. Drive forge-web as a user and exercise the api-host behind it."
fi

export BN_EXPLORE_REPO BN_EXPLORE_MAX BN_EXPLORE_LABEL BN_EXPLORE_FOCUS

exec sh "$(dirname "$0")/lib/explore.sh"
