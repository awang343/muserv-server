#!/usr/bin/env bash
# Example muserv downloader script.
#
# This is the contract every script in the `downloaders_path` directory
# (configured in config.toml) must follow to be runnable from the
# downloaders API / clients:
#
#   * Invoked as `<script> <url>` (argv[1] is the url the user typed in).
#   * `MUSERV_DOWNLOAD_DIR` is set (and is also the script's cwd) to a
#     directory *inside the target library*. Anything the script writes
#     there becomes a permanent part of the library once it's picked up by
#     the scan that runs automatically after the job finishes — same as any
#     other file dropped into the library by hand.
#   * Anything printed to stdout/stderr is shown to the user as job log
#     lines. There's no structured manifest to emit; new tracks are found by
#     the normal library scan, which reads tags straight off the files.
#   * Exit 0 on success. A non-zero exit marks the job failed, but the
#     library is still scanned afterward, so any files that *were*
#     downloaded are still added.
set -euo pipefail

url="${1:?usage: example.sh <url>}"
dest="${MUSERV_DOWNLOAD_DIR:-.}"

filename="$(basename "$url")"
filename="${filename:-download}"

echo "downloading $url"
curl -fsSL "$url" -o "$dest/$filename"
echo "saved $filename"
