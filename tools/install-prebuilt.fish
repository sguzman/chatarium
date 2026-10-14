#!/usr/bin/env fish
# Install the binary GitHub already compiled. Never invoke Cargo locally.
set -l here (path dirname (status filename))
set -l repo (path resolve "$here/..")
set -l archive_url https://github.com/sguzman/chatarium/releases/download/prebuilt-main/chatarium-linux-x86_64.tar.gz
set -l work (mktemp -d)
or exit 1

set -l archive "$work/chatarium-linux-x86_64.tar.gz"
set -l checksum "$work/chatarium-linux-x86_64.tar.gz.sha256"

if not command curl --fail --location --retry 4 --retry-all-errors --connect-timeout 20 --output "$archive" "$archive_url"
    echo "Failed downloading Chatarium's GitHub-built binary." >&2
    exit 1
end
if not command curl --fail --location --retry 4 --retry-all-errors --connect-timeout 20 --output "$checksum" "$archive_url.sha256"
    echo "Failed downloading the checksum for Chatarium's binary." >&2
    exit 1
end
if not command sh -c 'cd "$1" && sha256sum --check chatarium-linux-x86_64.tar.gz.sha256' sh "$work"
    echo "Checksum verification failed." >&2
    exit 1
end
if not command tar -xzf "$archive" -C "$work" chatarium-desktop COMMIT
    echo "Failed extracting Chatarium's prebuilt binary." >&2
    exit 1
end
set -l expected (command git -C "$repo" rev-parse HEAD)
set -l actual (string trim (command cat "$work/COMMIT"))
if test "$actual" != "$expected"
    echo "Published binary is for $actual, but local checkout is $expected; not installing a mismatched version." >&2
    exit 1
end
if not test -s "$work/chatarium-desktop"
    echo "Prebuilt binary is empty." >&2
    exit 1
end
command mkdir -p "$repo/target/release"
or exit 1
set -l staged "$repo/target/release/.chatarium-desktop-$fish_pid"
command install -m 0755 "$work/chatarium-desktop" "$staged"
or exit 1
command mv -f "$staged" "$repo/target/release/chatarium-desktop"
or exit 1
command rm -rf "$work"
echo "Installed GitHub-built Chatarium $actual to $repo/target/release/chatarium-desktop (no local Rust build)."
