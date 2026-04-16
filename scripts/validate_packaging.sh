#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
work_dir=$(mktemp -d)
trap 'rm -rf "$work_dir"' EXIT

cd "$repo_root"

bash -n scripts/release/build-release-archive.sh
bash -n scripts/aur/publish_aur_package.sh
bash -n scripts/aur/generate_srcinfo.sh
python - <<'PY'
from pathlib import Path
source = Path('scripts/aur/render_orators_bin_pkgbuild.py').read_text()
compile(source, 'scripts/aur/render_orators_bin_pkgbuild.py', 'exec')
PY

archive_path=$(./scripts/release/build-release-archive.sh --output-dir "$work_dir/dist")
test -f "$archive_path"
test -f "${archive_path}.sha256"

./scripts/aur/render_orators_bin_pkgbuild.py \
  --version 0.1.0 \
  --source-url https://example.invalid/orators-v0.1.0-x86_64-unknown-linux-gnu.tar.gz \
  --sha256 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef \
  --output-dir "$work_dir/orators-bin"
./scripts/aur/generate_srcinfo.sh "$work_dir/orators-bin/PKGBUILD" > "$work_dir/orators-bin/.SRCINFO"
./scripts/aur/generate_srcinfo.sh packaging/aur/orators-git/PKGBUILD > "$work_dir/orators-git.SRCINFO"

test -s "$work_dir/orators-bin/.SRCINFO"
test -s "$work_dir/orators-git.SRCINFO"
