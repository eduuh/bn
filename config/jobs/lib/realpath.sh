# Sourced helper: a portable `readlink -f`. macOS/BSD `readlink` has no `-f`, so jobs that need
# to canonicalize a path (resolve symlinks to an absolute path) can't rely on it cross-platform.
# Source it from a job:  . "$(dirname "$0")/lib/realpath.sh"
#
# POSIX sh; never run directly. Safe under `set -u`.

# bn_realpath <path> — echo the canonical absolute path of <path>, following symlinks on the
# final component. Portable substitute for GNU `readlink -f`. Returns nonzero if the containing
# directory can't be resolved. Prefer a real `readlink -f`/`realpath` when present (fast + handles
# every edge), else fall back to a symlink-following loop plus `cd`/`pwd -P`.
bn_realpath() {
  _bn_rp_path=$1

  if readlink -f "$_bn_rp_path" 2>/dev/null; then
    return 0
  fi
  if command -v realpath >/dev/null 2>&1; then
    realpath "$_bn_rp_path" 2>/dev/null && return 0
  fi

  # Follow symlinks on the final component (one or more levels), resolving relative targets
  # against their link's directory.
  while [ -L "$_bn_rp_path" ]; do
    _bn_rp_target=$(readlink "$_bn_rp_path") || return 1
    case $_bn_rp_target in
      /*) _bn_rp_path=$_bn_rp_target ;;
      *)  _bn_rp_path=$(dirname "$_bn_rp_path")/$_bn_rp_target ;;
    esac
  done

  _bn_rp_dir=$(CDPATH= cd -- "$(dirname "$_bn_rp_path")" 2>/dev/null && pwd -P) || return 1
  _bn_rp_base=$(basename "$_bn_rp_path")
  case $_bn_rp_base in
    .) printf '%s\n' "$_bn_rp_dir" ;;
    *) printf '%s/%s\n' "$_bn_rp_dir" "$_bn_rp_base" ;;
  esac
}
