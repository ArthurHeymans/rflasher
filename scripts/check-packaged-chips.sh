#!/usr/bin/env bash
set -euo pipefail

workspace_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$workspace_root"

cargo package --workspace --allow-dirty --no-verify

package_version=$(
  cargo metadata --no-deps --format-version 1 |
    python3 -c 'import json, sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "rflasher-chips"))'
)
archive="$workspace_root/target/package/rflasher-chips-$package_version.crate"

vendor_count=$(tar -tzf "$archive" | grep -cE '/data/vendors/[^/]+\.ron$')
if [[ "$vendor_count" -ne 22 ]]; then
  echo "expected 22 vendor database files in $archive, found $vendor_count" >&2
  exit 1
fi

package_dir=$(mktemp -d)
trap 'rm -rf "$package_dir"' EXIT

tar -xzf "$archive" -C "$package_dir"
extracted="$package_dir/rflasher-chips-$package_version"
mkdir -p "$extracted/.cargo"
cat >"$extracted/.cargo/config.toml" <<EOF
[patch.crates-io]
rflasher-chip-types = { path = "$workspace_root/crates/rflasher-chip-types" }
rflasher-chips-codegen = { path = "$workspace_root/crates/rflasher-chips-codegen" }
EOF

(
  cd "$extracted"
  cargo check --features static-chips
)
