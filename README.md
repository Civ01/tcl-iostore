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
- **Source stream for tcl-uassetvw** (`SourceStream`, default `View`): a package TC extracts
  to view it (F3, Quick View, Enter) or to drag it out gets a small NTFS stream `:uetoc.source`
  naming its container, the global's folder and the chunk (and for `$legacy` the engine version
  used), so the Lister plugin finds the game without asking. The file's content, size and dates
  don't change; nothing is written on volumes without named streams. `All` = every extracted
  file on any operation (F5 too), `Off` = never.
- **`.utoc`**: shows one text file, `<ContainerId>.txt`, describing the header, container
  header, signatures and every chunk (fixed-width tables for Lister's monospace font). Set
  `OpenUtocAsVirtualFile=0` to open `.utoc` files like the `.ucas`.
- A valid `global.utoc` + `global.ucas` must be in the container's folder or a parent folder
  (not above `Paks`).
- **Packing** (F5/Alt+F5 onto a container, F8 inside one): raw Zen files as the plugin extracts
  them. A file the game already has (any container under the global's folder) keeps its chunk
  ID, path and store entry and only the data is replaced. **New files** (UE 5.3+ games): a new
  package gets the chunk ID of the name inside it and a store entry built from its own imports;
  its `.ubulk`/`.uptnl`/`.m.ubulk` files go with it. Adding asks once per copy (and shows how the
  container's mount point changes, if it has to). Refused, with one message for the whole copy:
  `.ini`/`.locres` and other companion-pak files, legacy cooked `.uasset`+`.uexp` (later),
  new packages in pre-5.3 games, bulk files without their package. A new container takes TOC
  version, block size and compression from the game and gets an empty companion `.pak`
  (`WriteEmptyPak`). Changes are appended with an undo record (restored on the next open after
  a crash); small containers are rebuilt (`AutoCompactUnder`). Settings: the plugin's Configure
  dialog (Alt+F5 → Configure), stored in `[Pack]` of `uetoc.ini`.
- **Checks on add** (`ValidateOnAdd`, default Warn): a new package whose path doesn't give the
  name inside it (mod loaders and tools find packages by path) and materials (packed without
  shader maps) are listed in the question; Fix moves files to the path their name gives; Error
  refuses; Off skips.
- **Checks on replace** (`ValidateOnReplace`, default Warn): a packed package that differs from
  the game's in ways that can break loading (package name, imports vs the container header,
  removed or retyped exports other packages use) is reported before anything is written. Fix
  corrects the import list (UE 5.3+) and reports; Error refuses; Off skips the checks.
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
