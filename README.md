# tcl-iostore — UE IoStore packer plugin for Total Commander

`uetoc.wcx64`: a Total Commander 11+ (x64) packer plugin that browses, extracts and tests
Unreal Engine IoStore containers (`.utoc` + `.ucas`), UE 4.27 – 5.8. Read-only for now.
Sister of [tcl-uepak](https://github.com/Civ01/tcl-uepak) (legacy `.pak`). Design and
decisions: [SPEC.md](SPEC.md).

## What it does
- **`.ucas`**: lists the files of the container's directory index as stored (Zen
  `.uasset`/`.umap`, `.ubulk`, `.uptnl`, …), with the mount point shown as folders.
  Chunks without a path (container header, script objects, …) are in a hidden `$chunks`
  folder. Extract and test (chunk hashes) work on everything.
- **`.utoc`**: shows one text file, `<ContainerId>.txt`, describing the header, container
  header, signatures and every chunk (tab-separated, pastes into a spreadsheet). Set
  `OpenUtocAsVirtualFile=0` to open `.utoc` files like the `.ucas`.
- A valid `global.utoc` + `global.ucas` must be in the container's folder or a parent folder
  (not above `Paks`).
- Encrypted containers: AES key dialog; keys are remembered in `uetoc.keys.json`.
- Compression: Zlib, Gzip, LZ4, Zstd built in; Oodle with `deps\oo2core_N_win64.dll`
  (copy it from the game).

## Install
1. Build (`cargo build --release`) or take the CI artifact; the package holds
   `uetoc.wcx64`, `pluginst.inf`, `uetoc.ini`, `deps\README.txt`.
2. Open the zipped package in TC to install, or add it under Configuration → Plugins → Packer
   plugins. The installer associates `.ucas`; add `.utoc` for the same plugin as well.
3. Optional: copy `oo2core_9_win64.dll` from the game into `deps\`.

## Build
```
cargo build --release                                    # on Windows (MSVC or GNU)
cargo build --release --target x86_64-pc-windows-gnu     # cross-build from Linux (mingw-w64)
```
The container reader is the `iostore` crate in [Civ01/retoc](https://github.com/Civ01/retoc),
branch `tcl-iostore`.

## Tests
`cargo test` runs the unit tests. With the project's test containers:
```
IOSTORE_SAMPLES=/path/to/samples IOSTORE_OODLE=/path/to/oodle-lib-folder cargo test --release
```
(AES keys: `key.txt` next to a container.) Large real containers for performance checks:
`tools/fetch_isle_server.sh` (default, ~1 GB) and `tools/fetch_ark_server.sh` (chunks > 2 GB),
both anonymous Steam downloads through DepotDownloader. `tools/ucas_trim.py` makes small
upload copies of big containers.
