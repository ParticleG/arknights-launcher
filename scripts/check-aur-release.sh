#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
set -euo pipefail

repo=ParticleG/arknights-launcher
package=arknights-launcher
force=${1:-false}
if [[ $force != true && $force != false ]] || (( $# > 1 )); then
  echo 'Usage: bash scripts/check-aur-release.sh [true|false]' >&2
  exit 2
fi

# Choose the highest stable version, not the literal latest-by-date release.
tags=$(gh api --paginate "repos/$repo/releases?per_page=100" \
  --jq '.[] | select(.draft == false and .prerelease == false) | .tag_name')
version_pattern='(0|[1-9][0-9]*)[.](0|[1-9][0-9]*)[.](0|[1-9][0-9]*)'
latest=$(awk -v pattern="^v$version_pattern\$" '$0 ~ pattern { print substr($0, 2) }' <<<"$tags" \
  | LC_ALL=C sort -V | tail -n 1)
if [[ -z $latest ]]; then
  echo 'No stable vX.Y.Z GitHub release to publish.' >&2
  exit 0
fi

response=$(curl --fail --silent --show-error --proto '=https' \
  "https://aur.archlinux.org/rpc/v5/info?arg%5B%5D=$package")
published=$(jq -er --arg package "$package" '
  if .type != "multiinfo" or (.results | type) != "array" then error("Invalid AUR RPC response")
  elif .results == [] then ""
  elif (.results | length) == 1 and .results[0].Name == $package and (.results[0].Version | type) == "string"
  then .results[0].Version
  else error("Unexpected AUR package metadata") end
' <<<"$response")

if [[ -n $published ]]; then
  if [[ ! $published =~ ^$version_pattern-[1-9][0-9]*$ ]]; then
    printf 'Unsupported AUR version %s; refusing automatic publication.\n' "$published" >&2
    exit 1
  fi
  candidate=$latest-1
  highest=$(printf '%s\n' "$candidate" "$published" | LC_ALL=C sort -V | tail -n 1)
  if [[ $highest != "$candidate" || ( $candidate == "$published" && $force != true ) ]]; then
    printf 'AUR %s already covers GitHub release v%s.\n' "$published" "$latest" >&2
    exit 0
  fi
fi

printf '%s\n' "$latest"
