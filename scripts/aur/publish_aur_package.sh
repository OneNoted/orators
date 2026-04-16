#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOM'
Usage: publish_aur_package.sh <package-name> <source-dir> <commit-message>
EOM
}

if [[ $# -ne 3 ]]; then
  usage >&2
  exit 1
fi

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
package_name="$1"
source_dir=$(cd "$2" && pwd)
commit_message="$3"
packager_name="${AUR_PACKAGER_NAME:-Jonatan Jonasson}"
packager_email="${AUR_PACKAGER_EMAIL:-notes@madeingotland.com}"
repo_url="ssh://aur@aur.archlinux.org/${package_name}.git"
work_dir=$(mktemp -d)
repo_dir="$work_dir/$package_name"
trap 'rm -rf "$work_dir"' EXIT

GIT_SSH_COMMAND=${GIT_SSH_COMMAND:-ssh}
export GIT_SSH_COMMAND

if git ls-remote "$repo_url" >/dev/null 2>&1; then
  git clone "$repo_url" "$repo_dir"
else
  git init "$repo_dir"
  git -C "$repo_dir" remote add origin "$repo_url"
fi

rsync -a --delete --exclude '.git/' "$source_dir/" "$repo_dir/"
(
  cd "$repo_dir"
  "${repo_root}/scripts/aur/generate_srcinfo.sh" "$PWD/PKGBUILD" > .SRCINFO
  git config user.name "$packager_name"
  git config user.email "$packager_email"
  git add PKGBUILD .SRCINFO *.install
  if git diff --cached --quiet; then
    echo "No AUR changes to publish for ${package_name}."
    exit 0
  fi
  git commit -m "$commit_message"
  git push origin HEAD:master
)
