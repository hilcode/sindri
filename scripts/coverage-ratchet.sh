#!/usr/bin/env bash
#
# Enforce a non-decreasing floor on total test coverage.
#
# Reads a `cargo llvm-cov report` text report on stdin and re-emits it with the
# TOTAL line annotated: a metric that improved on its recorded minimum is marked
# with ↑↑↑ and one that dropped below it with ↓↓↓. Raises the minimums in the
# properties file when coverage improves, and exits non-zero when any metric
# drops below its minimum.
set -euo pipefail

PROPERTIES_FILE='.coverage.properties'
# Metrics to enforce, in the order the llvm-cov TOTAL line lists them.
METRICS=(regions functions lines)

format_percent() { awk -v hundredths="$1" 'BEGIN { printf "%.2f", hundredths / 100 }'; }

main() {
    local report metric value index
    local -A current_hundredths minimum_hundredths

    report=$(cat)

    # Current coverage as integer hundredths of a percent (95.11% -> 9511), read
    # straight from the percentages on the TOTAL line.
    local -a percentages
    read -r -a percentages < <(awk '$1 == "TOTAL" {
        line = ""
        for (field = 1; field <= NF; field++)
            if ($field ~ /%$/) { value = $field; sub(/%/, "", value); line = line sprintf("%d ", value * 100 + 0.5) }
        print line
    }' <<<"$report")
    index=0
    for metric in "${METRICS[@]}"; do
        current_hundredths["$metric"]="${percentages[index]}"
        index=$((index + 1))
    done

    # Recorded minimums as integer hundredths. A missing file or key means 0, so
    # the first run bootstraps the file from current coverage.
    for metric in "${METRICS[@]}"; do
        minimum_hundredths["$metric"]=0
    done
    if [[ -f "$PROPERTIES_FILE" ]]; then
        while IFS='=' read -r metric value; do
            [[ -z "$metric" || "$metric" == \#* ]] && continue
            minimum_hundredths["$metric"]=$(awk -v value="$value" 'BEGIN { printf "%d", value * 100 + 0.5 }')
        done < "$PROPERTIES_FILE"
    fi

    local regressed=false improved=false
    local -a indicators
    for metric in "${METRICS[@]}"; do
        if ((current_hundredths[$metric] < minimum_hundredths[$metric])); then
            indicators+=(' (↓↓↓)')
            regressed=true
        elif ((current_hundredths[$metric] > minimum_hundredths[$metric])); then
            indicators+=(' (↑↑↑)')
            improved=true
        else
            indicators+=('      ')
        fi
    done

    # Re-emit the report, replacing six padding spaces after each TOTAL-line
    # percentage with its (equally wide) indicator so the columns stay aligned.
    awk -v i1="${indicators[0]}" -v i2="${indicators[1]}" -v i3="${indicators[2]}" '
        BEGIN { indicator[1] = i1; indicator[2] = i2; indicator[3] = i3 }
        $1 == "TOTAL" {
            count = split($0, pieces, "%")
            annotated = pieces[1]
            for (piece = 2; piece <= count; piece++) {
                segment = pieces[piece]
                if ((piece - 1) in indicator) { sub(/^      /, "", segment); segment = indicator[piece - 1] segment }
                annotated = annotated "%" segment
            }
            print annotated
            next
        }
        { print }
    ' <<<"$report"

    if [[ "$regressed" == true ]]; then
        exit 1
    fi
    if [[ "$improved" == true ]]; then
        {
            for metric in "${METRICS[@]}"; do
                printf '%s=%s\n' "$metric" "$(format_percent "${current_hundredths[$metric]}")"
            done
        } > "$PROPERTIES_FILE"
    fi
}

main
