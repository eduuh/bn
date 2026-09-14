#!/usr/bin/env bash
# explore-bn — the bn lane of the explore pass. Autonomous.
#
# The per-repo settings live HERE rather than on the agent manifest's `run:` line, and that is the
# whole point of this file. The lane is fired by a timer through the scheduler, never from a shell,
# so the script is the one place its settings are always set and reviewable in git. On a command
# line, `run: "BN_EXPLORE_REPO=bn … bn run explore"` reached the driver with nothing set and exited 2.
#
# The agent manifest carries no `run:` at all now, so `bn run explore-bn` resolves straight to this
# script by name.
#
# Blast radius: bn's dashboard drives the machine's REAL fleet — there is no throwaway instance to
# point at — so the boundary is the action, not the environment. Read everything, exercise the safe
# and self-reverting writes, and stop at anything that steers, relaunches, kills or lands.
set -u

BN_EXPLORE_REPO=bn
BN_EXPLORE_MAX="${BN_EXPLORE_MAX:-5}"

if [ -z "${BN_EXPLORE_FOCUS:-}" ]; then
  BN_EXPLORE_FOCUS="the bn-web dashboard on 127.0.0.1:7791 -- the instance that bn build run starts for you, never 7770, which is the dashboard the user is looking at -- and the bn CLI. READ-ONLY and self-reverting writes only: never steer, relaunch, kill or land a live agent, and never act on a session you did not create. The fleet you are driving is the real one."
fi

export BN_EXPLORE_REPO BN_EXPLORE_MAX BN_EXPLORE_FOCUS

exec sh "$(dirname "$0")/lib/explore.sh"
