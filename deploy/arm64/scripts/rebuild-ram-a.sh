#!/usr/bin/env bash
set -euo pipefail

cd /opt/RAM-A
git fetch upstream refs/merge-requests/18/head
git checkout -B pr18 FETCH_HEAD
cargo test --manifest-path ram-a-mem/Cargo.toml -p memory-pipeline --test offline_pipeline
cargo build --manifest-path ram-a-mem/Cargo.toml --release --locked -p memory-mcp
install -m 0755 ram-a-mem/target/release/ram-a-mem /usr/local/bin/ram-a-mem.new
mv -f /usr/local/bin/ram-a-mem.new /usr/local/bin/ram-a-mem
git rev-parse HEAD >/opt/metadata/ram-a-commit
echo "RAM-A rebuilt from PR 18 commit $(cat /opt/metadata/ram-a-commit)"
