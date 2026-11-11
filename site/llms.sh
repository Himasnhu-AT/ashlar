#!/bin/sh
# Writes the AI-readable output after `mdbook build`:
#   book/llms.txt       an index of every page (llmstxt.org format)
#   book/llms-full.txt  every page's markdown, in SUMMARY.md order, with {{#include}}s resolved
#   book/<path>.md      each page's markdown, next to its HTML
# Run from the site/ folder: `sh llms.sh` (vercel.json does this after the build).
set -eu

SITE="${SITE_URL:-https://ashlar.himanshuat.com}"
SRC=src
OUT=book

[ -d "$OUT" ] || { echo "llms.sh: no $OUT/ folder; run mdbook build first" >&2; exit 1; }

# Print a page's markdown with every `{{#include path}}` line replaced by that file (paths are
# relative to the page, as mdBook resolves them).
resolve() {
    dir=$(dirname "$1")
    while IFS= read -r line || [ -n "$line" ]; do
        case "$line" in
            '{{#include '*'}}')
                inc=${line#'{{#include '}
                inc=${inc%'}}'}
                if [ -f "$dir/$inc" ]; then
                    cat "$dir/$inc"
                else
                    echo "llms.sh: $1: cannot include $inc" >&2
                    exit 1
                fi
                ;;
            *) printf '%s\n' "$line" ;;
        esac
    done < "$1"
}

# The page URL for a source path: README.md is the folder's index; cleanUrls drops `.html`.
url_of() {
    case "$1" in
        README.md) printf '%s/\n' "$SITE" ;;
        */README.md) printf '%s/%s/\n' "$SITE" "${1%/README.md}" ;;
        *) printf '%s/%s\n' "$SITE" "${1%.md}" ;;
    esac
}

# The first sentence of the first paragraph of prose (not a heading, table, list, quote, code
# block, image or HTML).
describe() {
    awk '
        /^```/ { if (para != "") exit; code = !code; next }
        code { next }
        /^[[:space:]]*$/ { if (para != "") exit; next }
        para == "" && (/^[[:space:]]/ || /^#/ || /^\|/ || /^[-*>] / || /^[0-9]+\. / || /^</ || /^!\[/ || /^\{\{/) { next }
        { para = (para == "" ? $0 : para " " $0) }
        END {
            gsub(/`/, "", para); gsub(/\*\*/, "", para)
            i = index(para, ". ")
            if (i > 0) para = substr(para, 1, i)
            if (length(para) > 200) para = substr(para, 1, 197) "..."
            print para
        }
    '
}

# SUMMARY.md entries in order, as "section<TAB>title<TAB>path". Part headings (`# User guide`)
# start a section.
entries() {
    awk '
        /^# / && NR > 1 { section = substr($0, 3); next }
        /\[[^]]*\]\([^)]*\.md\)/ {
            t = $0; sub(/^[^[]*\[/, "", t); title = t; sub(/\].*$/, "", title)
            p = $0; sub(/^.*\]\(/, "", p); sub(/\).*$/, "", p)
            print section "\t" title "\t" p
        }
    ' "$SRC/SUMMARY.md"
}

tab=$(printf '\t')
full="$OUT/llms-full.txt"
index="$OUT/llms.txt"

{
    echo "# ashlar"
    echo
    echo "> A Rust application framework on egui: documents with undo and autosave, commands that drive menus, shortcuts, a palette, a CLI and an MCP server, background jobs, files and settings, native OS features, UI components, and a runtime with crash safety, telemetry and agent control."
    echo
    echo "The full text of every page is in $SITE/llms-full.txt, and each page's markdown is at its URL plus \`.md\`. The API reference is on https://docs.rs/ashlar-rs."
} > "$index"
: > "$full"

last=""
entries | while IFS="$tab" read -r section title path; do
    page="$SRC/$path"
    [ -f "$page" ] || { echo "llms.sh: $page is in SUMMARY.md but missing" >&2; exit 1; }
    text=$(resolve "$page")

    mkdir -p "$OUT/$(dirname "$path")"
    printf '%s\n' "$text" > "$OUT/$path"

    if [ "$section" != "$last" ]; then
        printf '\n## %s\n\n' "$section" >> "$index"
        last=$section
    fi
    desc=$(printf '%s\n' "$text" | describe)
    printf -- '- [%s](%s): %s\n' "$title" "$(url_of "$path")" "$desc" >> "$index"

    {
        printf '<!-- %s -->\n\n' "$(url_of "$path")"
        printf '%s\n\n' "$text"
    } >> "$full"
done

echo "llms.sh: wrote $index, $full and the page sources"
