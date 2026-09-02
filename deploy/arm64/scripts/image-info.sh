#!/usr/bin/env bash
set -euo pipefail

echo "architecture=$(uname -m)"
echo "xiaoo_nevra=$(rpm -q xiaoO)"
echo "xiaoo_sha256=$(cut -d' ' -f1 /opt/metadata/xiaoo.sha256)"
echo "ram_a_commit=$(cat /opt/metadata/ram-a-commit)"
echo "ram_a_binary=$(command -v ram-a-mem)"
echo "xiaoo_binary=$(command -v xiaoo)"
ram-a-mem --help | head -n 2
xiaoo --help | head -n 5
