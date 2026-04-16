#!/usr/bin/env python3
from __future__ import annotations

import argparse
from pathlib import Path
import shutil


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument('--version', required=True)
    parser.add_argument('--source-url', required=True)
    parser.add_argument('--sha256', required=True)
    parser.add_argument('--output-dir', required=True)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    repo_root = Path(__file__).resolve().parents[2]
    template_path = repo_root / 'packaging' / 'aur' / 'orators-bin' / 'PKGBUILD.in'
    install_path = repo_root / 'packaging' / 'aur' / 'orators-bin' / 'orators.install'
    output_dir = Path(args.output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)

    rendered = template_path.read_text()
    rendered = rendered.replace('@PKGVER@', args.version)
    rendered = rendered.replace('@SOURCE_URL@', args.source_url)
    rendered = rendered.replace('@SHA256@', args.sha256)

    (output_dir / 'PKGBUILD').write_text(rendered)
    shutil.copy2(install_path, output_dir / 'orators.install')


if __name__ == '__main__':
    main()
