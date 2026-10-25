#!/usr/bin/env bash
# Builds the release bundle: dist/mullvad-per-app-vpn-linux-x86_64.tar.gz (+ .sha256).
# Upload both files to a GitHub release; install.sh downloads and verifies them.
set -euo pipefail
cd "$(dirname "$0")/.."
NAME=mullvad-per-app-vpn
(cd rust && CARGO_RESOLVER_INCOMPATIBLE_RUST_VERSIONS=fallback cargo build --release)
rm -rf dist && mkdir -p "dist/$NAME"
cp vpn-ns vpn-ns-qt apps.conf install.sh LICENSE README.md "dist/$NAME/"
cp rust/target/release/vpn-ns-gui "dist/$NAME/"
cp -r rust/flags "dist/$NAME/flags"
(cd dist && tar --owner=0 --group=0 --sort=name -czf "$NAME-linux-x86_64.tar.gz" "$NAME" && sha256sum "$NAME-linux-x86_64.tar.gz" > "$NAME-linux-x86_64.tar.gz.sha256")
rm -rf "dist/$NAME"
ls -l dist
