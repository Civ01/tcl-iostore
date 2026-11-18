#!/usr/bin/env bash
# Default large real-world IoStore test set: The Isle Dedicated Server, branch "evrima"
# (Steam app 412680, depot 412681), anonymous Steam login, no credentials. ~1 GB.
# UE 5.x, TOC v8, Oodle, encrypted + signed (key: the TheIsle key.txt in the project's
# IOStore_UEVanilla_test_containers set).
#   pakchunk0  9,568 chunks
#   pakchunk1 18,449 chunks, an 836 MB chunk (streaming)
#   pakchunk2 36,575 chunks, 2 MB directory index (listing speed)
# For chunks > 2 GB use fetch_ark_server.sh instead.
# Usage: DEPOTDOWNLOADER=/path/to/DepotDownloader tools/fetch_isle_server.sh OUT_DIR
# "InitializeSteam failed" is usually a transient Steam connection error: run again.
set -euo pipefail
out="${1:?usage: fetch_isle_server.sh OUT_DIR}"
dd="${DEPOTDOWNLOADER:-DepotDownloader}"
here="$(cd "$(dirname "$0")" && pwd)"
"$dd" -app 412680 -depot 412681 -beta evrima -filelist "$here/isle_filelist.txt" -dir "$out"
echo "Containers in: $out/TheIsle/Content/Paks"
