#!/usr/bin/env bash
# Reclaim disk from idle worktrees: remove ONLY each worktree's rebuildable
# `target/` directory. Never touches source, branches, or the worktree itself.
#
# A worktree is skipped when it is the one you run this from, when any process
# has its working directory under it (a build, test or agent shell is there),
# when its `target/` was modified within --min-age-hours (default 24), or when
# you pass it with --keep. Default is a dry run; --apply deletes.
#
# Usage:
#   scripts/prune-worktree-targets.sh                 # dry run, list candidates
#   scripts/prune-worktree-targets.sh --apply         # delete them
#   scripts/prune-worktree-targets.sh --min-age-hours 6 --keep /path/to/wt
#
# lsof only sees processes you may inspect: another user's build is invisible
# unless you run as root, so on a shared machine confirm owners and --keep.
#
# Run it from any worktree of the repository. Other people's and other agents'
# worktrees count too: when in doubt about an owner, ask before --apply.
set -euo pipefail

apply=0
min_age_hours=24
keep=()
while [ $# -gt 0 ]; do
  case "$1" in
    --apply) apply=1 ;;
    --min-age-hours) min_age_hours="${2:?--min-age-hours needs a number}"; shift ;;
    --keep) keep+=("$(cd "${2:?--keep needs a path}" && pwd -P)"); shift ;;  # git lists real paths
    -h | --help) awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"; exit 0 ;;
    *) echo "prune-worktree-targets: unknown argument '$1'" >&2; exit 2 ;;
  esac
  shift
done
case "$min_age_hours" in '' | *[!0-9]*) echo "prune-worktree-targets: --min-age-hours must be a whole number" >&2; exit 2 ;; esac

here="$(git rev-parse --show-toplevel)"
# Fail closed: if we cannot see process working directories, we cannot tell
# which worktrees are in use, so never delete.
command -v lsof >/dev/null 2>&1 || { echo "prune-worktree-targets: lsof not found; refusing to guess which worktrees are in use" >&2; exit 3; }
cwds="$(lsof -d cwd -Fn 2>/dev/null | sed -n 's/^n//p')" || true
[ -n "$cwds" ] || { echo "prune-worktree-targets: lsof reported no working directories; refusing to guess" >&2; exit 3; }
freed_kb=0
failed=0

while IFS= read -r wt; do
  [ -n "$wt" ] && [ -d "$wt/target" ] || continue
  reason=""
  [ "$wt" = "$here" ] && reason="current worktree"
  for k in "${keep[@]:-}"; do [ -n "$k" ] && [ "$wt" = "$k" ] && reason="--keep"; done
  if [ -z "$reason" ] && { printf '%s\n' "$cwds" | grep -qxF "$wt" || printf '%s\n' "$cwds" | grep -qF "$wt/"; }; then
    reason="process cwd inside"
  fi
  if [ -z "$reason" ] && [ -n "$(find "$wt/target" -maxdepth 3 -mmin "-$((min_age_hours * 60))" -print -quit 2>/dev/null)" ]; then
    reason="target/ modified in the last ${min_age_hours}h"
  fi
  kb="$(du -sk "$wt/target" 2>/dev/null | cut -f1 || true)"
  kb="${kb:-0}"
  if [ -n "$reason" ]; then
    printf 'keep   %6s MB  %s  (%s)\n' "$((kb / 1024))" "$wt" "$reason"
    continue
  fi
  if [ "$apply" = 1 ]; then
    if rm -rf -- "${wt:?}/target"; then
      printf 'pruned %6s MB  %s\n' "$((kb / 1024))" "$wt"
    else
      printf 'FAILED %6s MB  %s  (rm error; continuing)\n' "$((kb / 1024))" "$wt"
      failed=1
      continue
    fi
  else
    printf 'would  %6s MB  %s\n' "$((kb / 1024))" "$wt"
  fi
  freed_kb=$((freed_kb + kb))
done < <(git worktree list --porcelain | sed -n 's/^worktree //p')

verb="would free"
[ "$apply" = 1 ] && verb="freed"
printf '%s %s MB\n' "$verb" "$((freed_kb / 1024))"
exit "$failed"
