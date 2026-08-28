#!/usr/bin/env bash
# Downloads audio from any site yt-dlp supports and extracts it as m4a with
# embedded metadata.
#
# Requires: yt-dlp, ffmpeg (for --extract-audio / --embed-metadata)
#
# See example.sh for the full downloader script contract.
set -euo pipefail

url="${1:?usage: yt-dlp-m4a.sh <url>}"
dest="${MUSERV_DOWNLOAD_DIR:-.}"

yt-dlp \
  --no-progress \
  --extract-audio \
  --audio-format m4a \
  --embed-metadata \
  -P "$dest" \
  -o "%(id)s.%(ext)s" \
  "$url"
