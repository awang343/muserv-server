#!/usr/bin/env bash
# Downloads audio from any site yt-dlp supports (YouTube, SoundCloud,
# Bandcamp, ...) and extracts it as mp3 with embedded metadata/artwork, laid
# out as Artist/Album/Track.mp3 so the library scan picks up sane tags even
# when the source doesn't tag its files.
#
# Requires: yt-dlp, ffmpeg (for --extract-audio / --embed-thumbnail)
#
# See example.sh for the full downloader script contract.
set -euo pipefail

url="${1:?usage: yt-dlp.sh <url>}"
dest="${MUSERV_DOWNLOAD_DIR:-.}"

yt-dlp \
  --no-progress \
  --extract-audio \
  --audio-format mp3 \
  --audio-quality 0 \
  --embed-metadata \
  --embed-thumbnail \
  -o "$dest/%(artist,uploader,channel)s/%(album,playlist_title,'Downloads')s/%(track,title)s.%(ext)s" \
  "$url"
