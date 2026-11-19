# tcl-iostore — UE IoStore packer plugin for Total Commander

`uetoc.wcx64`: a Total Commander 11+ (x64) packer plugin that browses, extracts and tests
Unreal Engine IoStore containers (`.utoc` + `.ucas`), UE 4.27 – 5.8, and patches them with
raw Zen files (replace, delete, new patch containers).
Sister of [tcl-uepak](https://github.com/Civ01/tcl-uepak) (legacy `.pak`). Design and
decisions: [SPEC.md](SPEC.md).

## What it does
- **`.ucas`**: lists the files of the container's directory index as stored (Zen
  `.uasset`/`.umap`, `.ubulk`, `.uptnl`, …), with the mount point shown as folders.
  Chunks without a path (container header, script objects, …) are in a hidden `$chunks`
  folder. Extract and test (chunk hashes) work on everything.
- **Legacy files**: the hidden `$legacy` folder holds the same files converted to legacy
  cooked files (`.uasset` + `.uexp`, `.ubulk`, …, rebuilt shader libraries,
  `scriptobjects.bin` in the global's view) with retoc's `to-legacy` code. Imports are
  resolved across every container under the global's folder. For unversioned packages
  whose engine version the container can't tell (UE 5.6+), a dialog asks once
  (`LegacyEngineVersion` in `uetoc.ini`).
  Imports of engine structs and enums are written with class `Object` (always
  loadable); `LegacyGuessScriptTypes=1` names them by a guess instead (nicer in viewers).
- **`.utoc`**: shows one text file, `<ContainerId>.txt`, describing the header, container
  header, signatures and every chunk (fixed-width tables for Lister's monospace font). Set
  `OpenUtocAsVirtualFile=0` to open `.utoc` files like the `.ucas`.
- A valid `global.utoc` + `global.ucas` must be in the container's folder or a parent folder
  (not above `Paks`).
- **Packing** (F5/Alt+F5 onto a container, F8 inside one): raw Zen files as the plugin extracts
  them. Every file must already exist in the game (any container under the global's folder):
  its chunk ID, path and store entry are reused and only the data is replaced. Anything else
  (new files, `.ini`/`.locres` and other companion-pak files, legacy cooked `.uasset`+`.uexp`)
  cancels the whole copy with one message. A new container takes TOC version, block size and
  compression from the game and gets an empty companion `.pak` (`WriteEmptyPak`). Changes are
  appended with an undo record (restored on the next open after a crash); small containers are
  rebuilt (`AutoCompactUnder`). Settings: the plugin's Configure dialog (Alt+F5 → Configure),
  stored in `[Pack]` of `uetoc.ini`.
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
