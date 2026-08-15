# Print a recipe's full description (its doc comment, minus the one-line summary `just --list` shows)
help recipe:
    #!/usr/bin/env bash
    set -euo pipefail
    awk -v recipe="{{ recipe }}" '
        { lines[NR] = $0 }
        END {
            target = 0
            for (i = 1; i <= NR; i++) {
                if (lines[i] ~ ("^" recipe "([ :]|$)")) { target = i; break }
            }
            if (!target) { print "no such recipe: " recipe > "/dev/stderr"; exit 1 }
            n = 0
            i = target - 1
            while (i >= 1) {
                line = lines[i]
                if (line ~ /^\[/) { i--; continue }
                if (line ~ /^#/) { n++; comments[n] = line; i--; continue }
                break
            }
            for (j = 1; j <= n; j++) reversed[j] = comments[n - j + 1]
            count = n - 1
            while (count >= 1 && reversed[count] ~ /^#[ \t]*$/) count--
            # No separate long description above the one-line `just --list` summary — fall back to
            # printing that summary line rather than nothing.
            if (count == 0 && n >= 1) {
                line = reversed[n]
                sub(/^#[ ]?/, "", line)
                print line
                exit
            }
            for (j = 1; j <= count; j++) {
                line = reversed[j]
                sub(/^#[ ]?/, "", line)
                print line
            }
        }
    ' "{{ justfile() }}"
