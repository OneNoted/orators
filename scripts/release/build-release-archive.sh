#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOM'
Usage: build-release-archive.sh [--version <semver>] [--target <triple>] [--output-dir <dir>]
EOM
}

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
version=''
target='x86_64-unknown-linux-gnu'
output_dir="$repo_root/dist"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      version="$2"
      shift 2
      ;;
    --target)
      target="$2"
      shift 2
      ;;
    --output-dir)
      output_dir="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if [[ -z "$version" ]]; then
  version=$(python - <<'PY'
import json, subprocess
metadata = json.loads(subprocess.check_output([
    'cargo', 'metadata', '--no-deps', '--format-version', '1'
], text=True))
for package in metadata['packages']:
    if package['name'] == 'orators':
        print(package['version'])
        break
else:
    raise SystemExit('could not resolve orators package version')
PY
)
fi

archive_root="orators-v${version}-${target}"
archive_path="$output_dir/${archive_root}.tar.gz"
checksum_path="${archive_path}.sha256"
stage_dir=$(mktemp -d)
trap 'rm -rf "$stage_dir"' EXIT

mkdir -p "$output_dir"
cd "$repo_root"

cargo build --locked --release --target "$target" -p orators
build_dir="target/${target}/release"

mkdir -p \
  "$stage_dir/$archive_root/bin" \
  "$stage_dir/$archive_root/systemd/user"

install -Dm755 "$build_dir/orators" "$stage_dir/$archive_root/bin/orators"
install -Dm755 "$build_dir/oratorsctl" "$stage_dir/$archive_root/bin/oratorsctl"
install -Dm755 "$build_dir/oratorsd" "$stage_dir/$archive_root/bin/oratorsd"
install -Dm644 packaging/systemd/user/oratorsd.service \
  "$stage_dir/$archive_root/systemd/user/oratorsd.service"
install -Dm644 README.md "$stage_dir/$archive_root/README.md"
install -Dm644 LICENSE "$stage_dir/$archive_root/LICENSE"

tar -C "$stage_dir" -czf "$archive_path" "$archive_root"
sha256sum "$archive_path" > "$checksum_path"
cat "$checksum_path" >&2

echo "$archive_path"
