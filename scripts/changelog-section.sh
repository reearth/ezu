#!/usr/bin/env bash
# Print one version's section of CHANGELOG.md, without its heading — the body
# of that version's GitHub release.
#
#   scripts/changelog-section.sh v0.10.0
#
# A missing or empty section is an error rather than an empty release note, so
# a release that nobody wrote up fails instead of shipping silently.
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: $0 <version>" >&2
  exit 2
fi

version=${1#v}
changelog=$(dirname "$0")/../CHANGELOG.md

section=$(awk -v v="$version" '
  # Headings look like "## 0.10.0 — 2026-09-16"; match the version as a whole
  # word so 0.1.0 never picks up 0.1.0-rc or 0.10.0.
  $0 ~ "^## " v "([^0-9.]|$)" { found = 1; next }
  found && /^## / { exit }
  found { print }
' "$changelog")

# Trim the blank lines that sit either side of the section in the file.
section=$(printf '%s\n' "$section" | sed -e '/./,$!d' | sed -e :a -e '/^\n*$/{$d;N;ba' -e '}')

if [ -z "$section" ]; then
  echo "CHANGELOG.md has no entry for $version" >&2
  exit 1
fi

printf '%s\n' "$section"
