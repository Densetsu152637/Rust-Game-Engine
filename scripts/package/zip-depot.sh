#!/usr/bin/env bash
# Archive a Steam depot directory (engine project package --format steam-depot) as a zip
# with the directory itself as the one top-level folder.
#
# usage: zip-depot.sh <depot-dir> <archive.zip>
#   <archive.zip> must not exist. macOS uses ditto, which keeps the symbolic links,
#   extended attributes and permissions an app bundle's code signature depends on.
#   Every other host uses Python's zipfile module, which stores the Unix modes (the
#   execute bit of the runtime) and needs no zip command on the Windows runner.
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: zip-depot.sh <depot-dir> <archive.zip>" >&2
  exit 2
fi
if [ ! -d "$1" ]; then
  echo "depot directory not found: $1" >&2
  exit 2
fi
if [ -e "$2" ]; then
  echo "archive must not exist: $2" >&2
  exit 2
fi
depot=$(cd "$1" && pwd)
archive=$(cd "$(dirname "$2")" && pwd)/$(basename "$2")
parent=$(dirname "$depot")
name=$(basename "$depot")

case "$(uname -s)" in
  Darwin)
    ditto -c -k --sequesterRsrc --keepParent "$depot" "$archive"
    ;;
  *)
    if command -v python3 >/dev/null 2>&1; then
      python=python3
    else
      python=python
    fi
    case "$(uname -s)" in
      MINGW* | MSYS* | CYGWIN*) archive=$(cygpath -m "$archive") ;;
    esac
    (cd "$parent" && "$python" -I -m zipfile -c "$archive" "$name")
    ;;
esac
ls -l "$archive"
