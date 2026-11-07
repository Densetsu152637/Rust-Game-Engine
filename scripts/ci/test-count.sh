#!/usr/bin/env bash
# Count tests from libtest output so CI can enforce a minimum number of passes.
#
#   test-count.sh count <log>...          print the total "passed" count
#   test-count.sh summary <log>...        print "passed failed ignored" totals
#   test-count.sh breakdown <log>...      print passed tests per test binary
#   test-count.sh gate <minimum> <file>[:<min>]...
#                                         sum count files; fail below <minimum>, or
#                                         when a file is below its own <min>
#
# libtest ends every test binary with a line such as
#   test result: ok. 12 passed; 0 failed; 3 ignored; 0 measured; 0 filtered out
# Ignored tests (GPU, audio device, native UI) are never counted as passed.
# A per-file minimum keeps one platform from regressing behind the others,
# which a sum alone would hide.
set -euo pipefail

# Regexes stay POSIX: macOS runs BSD sed, which has no \x1b or \b escapes.
escape=$(printf '\033')
strip_ansi() { sed -E "s/${escape}\[[0-9;]*[A-Za-z]//g"; }

# Sum the number captured before the given label on libtest result lines.
total() {
    local label=$1
    shift
    cat "$@" | strip_ansi | tr -d '\r' \
        | sed -nE "s/^test result: .* ([0-9]+) ${label}(;.*)?$/\1/p" \
        | awk '{ sum += $1 } END { print sum + 0 }'
}

usage() {
    sed -n '2,15p' "$0" >&2
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
    short=0
    for argument in "$@"; do
        file=$argument
        file_minimum=
        # Split at the last colon only when a number follows it.
        suffix=${argument##*:}
        if [ "$suffix" != "$argument" ] && [ -n "$suffix" ] && [ -z "${suffix//[0-9]/}" ]; then
            file=${argument%:*}
            file_minimum=$suffix
        fi
        if [ ! -f "$file" ]; then
            echo "error: $file is missing" >&2
            exit 1
        fi
        value=$(tr -d '[:space:]' <"$file")
        case "$value" in
        '' | *[!0-9]*)
            echo "error: $file does not contain a test count" >&2
            exit 1
            ;;
        esac
        if [ -n "$file_minimum" ]; then
            echo "$file: $value passed (minimum $file_minimum)"
            if [ "$value" -lt "$file_minimum" ]; then
                echo "error: $file has fewer than $file_minimum passed tests" >&2
                short=1
            fi
        else
            echo "$file: $value passed"
        fi
        sum=$((sum + value))
    done
    echo "total passed: $sum (minimum $minimum)"
    if [ "$sum" -lt "$minimum" ]; then
        echo "error: fewer than $minimum tests passed" >&2
        exit 1
    fi
    [ "$short" -eq 0 ] || exit 1
    ;;
*)
    usage
    ;;
esac
