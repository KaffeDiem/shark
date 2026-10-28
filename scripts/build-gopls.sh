#!/bin/sh
# CI owns this compiler; end users never compile or upgrade Go to install gopls.
set -eu
cd "$(dirname "$0")/.."
test "$(uname -s)-$(uname -m)" = Darwin-arm64 || { echo 'Build and verify shipped gopls on Apple Silicon macOS' >&2; exit 1; }
test "$(go version | awk '{print $3}')" = go1.27.3 || { echo 'Install Go 1.27.3 for the release build' >&2; exit 1; }
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
GOBIN="$stage" GOTOOLCHAIN=local GOOS=darwin GOARCH=arm64 CGO_ENABLED=0 go install -trimpath golang.org/x/tools/gopls@v0.24.0
python3 scripts/lsp-smoke.py go -- "$stage/gopls"
out=target/language-tools/gopls/0.24.0
mkdir -p "$out"
cp "$stage/gopls" "$out/gopls"
cp "$(go env GOMODCACHE)/golang.org/x/tools/gopls@v0.24.0/LICENSE" "$out/LICENSE"
go version -m "$out/gopls" > "$out/build.txt"
shasum -a 256 "$out/gopls" | awk '{print $1}' > "$out/gopls.sha256"
tar -czf target/language-tools/gopls-0.24.0-darwin-arm64.tar.gz -C "$out" gopls gopls.sha256 LICENSE build.txt
shasum -a 256 target/language-tools/gopls-0.24.0-darwin-arm64.tar.gz > target/language-tools/gopls-0.24.0-darwin-arm64.tar.gz.sha256
