#!/usr/bin/env bash
# Count tests from libtest output so CI can enforce a minimum number of passes.
#
#   test-count.sh count <log>...          print the total "passed" count
#   test-count.sh summary <log>...        print "passed failed ignored" totals
#   test-count.sh breakdown <log>...      print passed tests per test binary
#   test-count.sh gate <minimum> <file>...  sum count files, fail below <minimum>
#
# libtest ends every test binary with a line such as
#   test result: ok. 12 passed; 0 failed; 3 ignored; 0 measured; 0 filtered out
# Ignored tests (GPU, audio device, native UI) are never counted as passed.
set -euo pipefail

strip_ansi() { sed -E 's/\x1b\[[0-9;]*[A-Za-z]//g'; }

# Sum the number captured before the given label on libtest result lines.
total() {
    local label=$1
    shift
    cat "$@" | strip_ansi | tr -d '\r' \
        | sed -nE "s/^test result: .* ([0-9]+) ${label}\b.*/\1/p" \
        | awk '{ sum += $1 } END { print sum + 0 }'
}

usage() {
    sed -n '2,11p' "$0" >&2
    exit 2
}

[ $# -ge 1 ] || usage
command=$1
shift
case "$command" in
count)
    [ $# -ge 1 ] || usage
    total passed "$@"
    ;;
summary)
    [ $# -ge 1 ] || usage
    echo "$(total passed "$@") $(total failed "$@") $(total ignored "$@")"
    ;;
breakdown)
    [ $# -ge 1 ] || usage
    cat "$@" | strip_ansi | tr -d '\r' | awk '
        /^ *Running / {
            name = $0
            sub(/^.*[\\\/]deps[\\\/]/, "", name)
            sub(/-[0-9a-f]+(\.exe)?\)$/, "", name)
        }
        /^ *Doc-tests / { name = "doctests " $2 }
        /^test result: / {
            for (i = 2; i <= NF; i++) if ($i == "passed;") count[name] += $(i - 1)
        }
        END { for (key in count) printf "%s %d\n", key, count[key] }' | sort
    ;;
gate)
    [ $# -ge 2 ] || usage
    minimum=$1
    shift
    sum=0
    for file in "$@"; do
        value=$(tr -d '[:space:]' <"$file")
        case "$value" in
        '' | *[!0-9]*)
            echo "error: $file does not contain a test count" >&2
            exit 1
            ;;
        esac
        echo "$file: $value passed"
        sum=$((sum + value))
    done
    echo "total passed: $sum (minimum $minimum)"
    if [ "$sum" -lt "$minimum" ]; then
        echo "error: fewer than $minimum tests passed" >&2
        exit 1
    fi
    ;;
*)
    usage
    ;;
esac
