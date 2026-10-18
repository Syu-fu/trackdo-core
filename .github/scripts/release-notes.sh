#!/usr/bin/env bash
# Prints the CHANGELOG section for a release tag, after checking that the tag
# matches the workspace version.
#
# Usage: .github/scripts/release-notes.sh v0.2.0
set -euo pipefail

tag="${1:?usage: $0 <tag>}"
version="${tag#v}"

manifest_version="$(
  awk '/^\[workspace\.package\]/ { in_package = 1; next }
       /^\[/ { in_package = 0 }
       in_package && /^version *=/ { gsub(/[" ]/, ""); sub(/^version=/, ""); print; exit }' Cargo.toml
)"
if [[ "$version" != "$manifest_version" ]]; then
  echo "tag $tag does not match workspace version $manifest_version" >&2
  exit 1
fi

# The section runs from its "## [x.y.z]" heading to the next heading or link
# definition; leading and trailing blank lines are dropped.
notes="$(
  awk -v heading="## [$version]" '
    index($0, heading) == 1 { in_section = 1; next }
    in_section && (/^## \[/ || /^\[[^]]+\]: /) { exit }
    in_section { print }' CHANGELOG.md | sed -e '/./,$!d'
)"
if [[ -z "${notes//[[:space:]]/}" ]]; then
  echo "CHANGELOG.md has no notes for $version" >&2
  exit 1
fi

printf '%s\n' "$notes"
