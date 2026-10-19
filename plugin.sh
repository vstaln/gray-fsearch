#!/bin/sh
here="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
sys="$(uname -s)"
case "$sys" in
MINGW* | MSYS* | CYGWIN*) rel="x86_64-pc-windows-msvc/gray-fsearch.exe" ;;
Darwin*)
    case "$(uname -m)" in
    arm64) rel="aarch64-apple-darwin/gray-fsearch" ;;
    *) rel="x86_64-apple-darwin/gray-fsearch" ;;
    esac ;;
*)
    case "$(uname -m)" in
    aarch64 | arm64) rel="aarch64-unknown-linux-musl/gray-fsearch" ;;
    *) rel="x86_64-unknown-linux-musl/gray-fsearch" ;;
    esac ;;
esac
if [ -x "$here/$rel" ]; then exec "$here/$rel" "$@"; fi
for cand in "$here/target/release/gray-fsearch.exe" "$here/target/release/gray-fsearch" "$here/target/debug/gray-fsearch.exe" "$here/target/debug/gray-fsearch"; do
    if [ -x "$cand" ]; then exec "$cand" "$@"; fi
done
echo "gray-fsearch binary not found in $here" >&2
exit 1
