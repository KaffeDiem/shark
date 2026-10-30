#!/bin/sh
# CI owns this compiler; end users never compile or upgrade Go to install gopls.
set -eu
cd "$(dirname "$0")/.."
# The release workflow installs this Go; the gopls pin lives in src/lsp/tools.rs.
GO_VERSION=1.27.3
GOPLS_VERSION=$(sed -n 's/^const GOPLS_VERSION: &str = "\(.*\)";$/\1/p' src/lsp/tools.rs)
test -n "$GOPLS_VERSION" || { echo 'GOPLS_VERSION is missing from src/lsp/tools.rs' >&2; exit 1; }
test "$(uname -s)-$(uname -m)" = Darwin-arm64 || { echo 'Build and verify shipped gopls on Apple Silicon macOS' >&2; exit 1; }
test "$(go version | awk '{print $3}')" = "go$GO_VERSION" || { echo "Install Go $GO_VERSION for the release build" >&2; exit 1; }
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
GOBIN="$stage" GOTOOLCHAIN=local GOOS=darwin GOARCH=arm64 CGO_ENABLED=0 go install -trimpath "golang.org/x/tools/gopls@v$GOPLS_VERSION"
python3 scripts/lsp-smoke.py go -- "$stage/gopls"
out=target/language-tools/gopls/$GOPLS_VERSION
archive=gopls-$GOPLS_VERSION-darwin-arm64.tar.gz
mkdir -p "$out"
cp "$stage/gopls" "$out/gopls"
cp "$(go env GOMODCACHE)/golang.org/x/tools/gopls@v$GOPLS_VERSION/LICENSE" "$out/LICENSE"
go version -m "$out/gopls" > "$out/build.txt"
shasum -a 256 "$out/gopls" | awk '{print $1}' > "$out/gopls.sha256"
tar -czf "target/language-tools/$archive" -C "$out" gopls gopls.sha256 LICENSE build.txt
# A bare file name, so `shasum -c` works next to the downloaded release asset.
(cd target/language-tools && shasum -a 256 "$archive" > "$archive.sha256")
