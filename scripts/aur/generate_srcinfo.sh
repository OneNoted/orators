#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "Usage: generate_srcinfo.sh <PKGBUILD>" >&2
  exit 1
fi

pkgbuild=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
pkgdir=$(dirname "$pkgbuild")

# shellcheck disable=SC1090
source "$pkgbuild"

pkgbase_value="${pkgbase:-$pkgname}"

print_repeated() {
  local key="$1"
  shift
  local value
  for value in "$@"; do
    printf '\t%s = %s\n' "$key" "$value"
  done
}

printf 'pkgbase = %s\n' "$pkgbase_value"
printf '\tpkgdesc = %s\n' "$pkgdesc"
printf '\tpkgver = %s\n' "$pkgver"
printf '\tpkgrel = %s\n' "$pkgrel"
printf '\turl = %s\n' "$url"
print_repeated 'arch' "${arch[@]}"
print_repeated 'license' "${license[@]}"
if declare -p makedepends >/dev/null 2>&1; then
  print_repeated 'makedepends' "${makedepends[@]}"
fi
if declare -p depends >/dev/null 2>&1; then
  print_repeated 'depends' "${depends[@]}"
fi
if declare -p provides >/dev/null 2>&1; then
  print_repeated 'provides' "${provides[@]}"
fi
if declare -p conflicts >/dev/null 2>&1; then
  print_repeated 'conflicts' "${conflicts[@]}"
fi
if [[ -n ${install:-} ]]; then
  printf '\tinstall = %s\n' "$install"
fi
print_repeated 'source' "${source[@]}"
print_repeated 'sha256sums' "${sha256sums[@]}"
printf '\n'
printf 'pkgname = %s\n' "$pkgname"
