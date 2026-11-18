#!/usr/bin/env bash
# Optional large set (chunks > 2 GB): ARK: Survival Ascended Dedicated
# Server (Steam app 2430930, depot 2430931), anonymous Steam login, no credentials.
# Only the files in ark_filelist.txt (~3.5 GB): global, every .utoc, and the .ucas of
#   pakchunk0  (162,944 chunks, 8.3 MB directory index: listing speed)
#   pakchunk10 (a 3.5 GB chunk: streaming)
#   pakchunk11 (small, quick full check)
# Usage: DEPOTDOWNLOADER=/path/to/DepotDownloader tools/fetch_ark_server.sh OUT_DIR
# DepotDownloader: https://github.com/SteamRE/DepotDownloader (needs .NET 10).
# The depot follows game updates, so chunk counts and sizes can change.
# "InitializeSteam failed" is usually a transient Steam connection error: run again.
set -euo pipefail
out="${1:?usage: fetch_ark_server.sh OUT_DIR}"
dd="${DEPOTDOWNLOADER:-DepotDownloader}"
here="$(cd "$(dirname "$0")" && pwd)"
"$dd" -app 2430930 -depot 2430931 -filelist "$here/ark_filelist.txt" -dir "$out"
echo "Containers in: $out/ShooterGame/Content/Paks"
