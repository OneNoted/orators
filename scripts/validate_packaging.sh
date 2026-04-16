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

fake_cargo_dir="$work_dir/fake-cargo"
fake_cargo_log="$work_dir/fake-cargo.log"
fake_target='aarch64-unknown-linux-gnu'
mkdir -p "$fake_cargo_dir"
cat > "$fake_cargo_dir/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$1" == metadata ]]; then
  cat <<'JSON'
{"packages":[{"name":"orators","version":"0.1.0"}]}
JSON
  exit 0
fi
if [[ "$1" == build ]]; then
  printf '%s
' "$*" > "$FAKE_CARGO_LOG"
  build_dir="$PWD/target/$FAKE_CARGO_TARGET/release"
  mkdir -p "$build_dir"
  for bin in orators oratorsctl oratorsd; do
    printf '#!/usr/bin/env bash
exit 0
' > "$build_dir/$bin"
    chmod +x "$build_dir/$bin"
  done
  exit 0
fi
echo "unexpected cargo invocation: $*" >&2
exit 1
EOF
chmod +x "$fake_cargo_dir/cargo"
FAKE_CARGO_LOG="$fake_cargo_log" FAKE_CARGO_TARGET="$fake_target" PATH="$fake_cargo_dir:$PATH" ./scripts/release/build-release-archive.sh   --version 0.1.0   --target "$fake_target"   --output-dir "$work_dir/fake-dist" >/dev/null
rg --fixed-strings -- '--target' "$fake_cargo_log"
rg --fixed-strings -- "$fake_target" "$fake_cargo_log"
tar -tzf "$work_dir/fake-dist/orators-v0.1.0-${fake_target}.tar.gz" | rg --fixed-strings "orators-v0.1.0-${fake_target}/bin/orators"
