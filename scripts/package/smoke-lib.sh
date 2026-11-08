#!/usr/bin/env bash
# Shared helpers for the scripts/package/smoke-*.sh scripts. Source it; do not run it.
#
# The smoke scripts check a finished package the way a user receives it. Their
# checks are grouped into phases: every phase runs even when an earlier one failed
# (so one run reports every broken check, as separate CI steps did), and the script
# exits 1 if any phase failed. Callers must not use `set -e` at the top level; a
# phase runs in a subshell with `set -e`, which `if`/`||` would silently disable.

phase_failures=0

# phase <title> <function> [args...]: run one group of checks, stop at its first
# failing command, count it and carry on with the next phase.
phase() {
  local title=$1 status=0
  shift
  printf '::group::%s\n' "$title"
  ( set -eu; "$@" )
  status=$?
  printf '::endgroup::\n'
  if [ "$status" -ne 0 ]; then
    printf 'FAILED (%s): %s\n' "$status" "$title" >&2
    phase_failures=$((phase_failures + 1))
  fi
  return 0
}

# finish: report the phase count and exit non-zero when any phase failed.
finish() {
  if [ "$phase_failures" -ne 0 ]; then
    printf '%s smoke phase(s) failed\n' "$phase_failures" >&2
    exit 1
  fi
  echo "all smoke phases passed"
}

# is_windows: true under Git for Windows / MSYS / Cygwin bash.
is_windows() {
  case "$(uname -s)" in MINGW* | MSYS* | CYGWIN*) return 0 ;; *) return 1 ;; esac
}

# neutral_dir: a working directory unrelated to the package and the checkout, so a
# launcher cannot find game/project.json by accident.
neutral_dir() {
  printf '%s' "${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
}

# extract_zip <archive.zip> <destination>: extract a zip into an existing directory.
# Windows uses PowerShell's Expand-Archive; macOS uses ditto (which restores the links,
# extended attributes and modes a code signature depends on); other Unix hosts use unzip,
# which restores the Unix modes stored in the archive.
extract_zip() {
  if is_windows; then
    # Windows PowerShell 5.1 reports Expand-Archive errors as non-terminating, so a
    # partly corrupt zip would exit 0; "Stop" turns them into a failing exit code.
    # shellcheck disable=SC2016  # $env: is PowerShell syntax, not a shell expansion
    SMOKE_ZIP=$(cygpath -w "$1") SMOKE_DEST=$(cygpath -w "$2") \
      powershell.exe -NoProfile -NonInteractive -Command \
      '$ErrorActionPreference = "Stop"; Expand-Archive -LiteralPath $env:SMOKE_ZIP -DestinationPath $env:SMOKE_DEST'
  elif [ "$(uname -s)" = Darwin ]; then
    ditto -x -k "$1" "$2"
  else
    unzip -q "$1" -d "$2"
  fi
}
