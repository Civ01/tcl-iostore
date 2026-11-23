# tcl-iostore — Specification

Total Commander 11+ x64 packer plugin (`uetoc.wcx64`) for Unreal Engine IoStore containers
(`.utoc` + `.ucas`). Sister of [tcl-uepak](https://github.com/Civ01/tcl-uepak) (`uepak.wcx64`,
legacy `.pak`), whose skeleton, conventions and lessons it reuses; companion of
[tcl-uassetvw](https://github.com/Civ01/tcl-uassetvw) (Lister plugin for `.uasset`).

Spec version: v4 (2026-11-23). This document describes the plugin **as built**; history and
superseded plans were removed (see git history before v4). Decisions are the user's unless marked
*(default)* = chosen by me, open to change.

---

## 0. Status and roadmap

| # | Milestone | Status |
|---|---|---|
| R1–R4 | Reading: container reader (partitions, streaming, AES, Oodle, typed errors), listing, `$chunks`, `.utoc` info file, extract + test with hash checks, keys, game-profile hook | done 2026-11-19 |
| N3 | Legacy view `$legacy`: Zen packages as legacy `.uasset/.uexp/…`, shader libraries, `scriptobjects.bin` (§7) | done 2026-11-19 |
| N3b | UE 5.8 specifics: 8-byte shader hashes, cooked import `PackageName` (§7.2) | done 2026-11-19 |
| N1 | Packing raw Zen files: replace/delete in containers, new patch containers, Configure dialog (§8) | done 2026-11-19/20 |
| — | `ValidateOnReplace` (§8.5) | done 2026-11-20 |
| N7 | `:uetoc.source` NTFS stream for tcl-uassetvw (§9) | done 2026-11-20 |
| N1b | New files: packages (5.3+) and their bulk files, `ValidateOnAdd`, mount-point question (§8.4, §8.6) | done 2026-11-20 |
| — | New-container mount-point dialog, paths that follow the mount point (§8.3) | done 2026-11-23 |

Remaining work: §15.

### Non-goals
- New packages for pre-5.3 games (their packages don't name their imports; §8.4).
- New raw chunk files other than packages and bulk files (shader libraries, …) — possibly with N2.
- UE 4.25/4.26 containers (TOC v1/v2): may work, not tested or promised.
- Showing the companion `.pak` contents (tcl-uepak does that). No `.pak` writing except the empty
  companion `.pak` (§8.3); repak is not linked into the plugin.

---

## 1. Formats (verified)

Against UE 5.8.4 source (`Core/Internal/IO/IoStore.h`, `Core/Private/IO/IoStore.cpp`,
`IoStoreUtilities/Private/IoStoreWriter.cpp`, `PakFile/Private/IPlatformFilePak.cpp`,
`AsyncLoading2.cpp`, `PackageStoreOptimizer.cpp`) and the test containers.

| TOC version | Name | Engine | Samples |
|---|---|---|---|
| 3 | PartitionSize | 4.27 | vanilla 4.27 (Oodle), encrypted 4.27 |
| 4 | PerfectHash | early 5.0 previews | none |
| 5 | PerfectHashWithOverflow | 5.0–5.3 | VanySlash 5.0 (trimmed), vanilla 5.1–5.3 |
| 6 | OnDemandMetaData | 5.4 | vanilla 5.4, AngelScript 5.4, G1R (Oodle) |
| 7 | RemovedOnDemandMetaData | unreleased | none |
| 8 | ReplaceIoChunkHashWithIoHash | 5.5–5.8 | vanilla 5.5–5.8, 5.8 Zlib/Gzip/LZ4, The Isle 5.6 (encrypted, signed), ARK, The Isle server |
| 9, 10 | AddedSourceHashes, ContainerEncryptionMethod | after 5.8.4 | none; refused: "TOC version N is newer than this plugin supports" |

- Header 0x90 bytes, magic `-==--==--==--==-`; a header-size or entry-size mismatch is an error.
- Container header versions −1 PreInitial … 5 SoftPackageReferencesOffset (5.6+). 5.6/5.7/5.8
  share version 5; 5.3+ (≥ NoExportInfo) store entries hold imported packages + shader-map
  hashes, older ones also export counts and load order.
- Chunk hashes: SHA-1 of the uncompressed chunk in 4.27; BLAKE3 truncated to 20 bytes in 5.0–5.8.
- Blocks: disk size padded to 16 with `buf[(i - size) % size]`, AES-256-ECB over the padded block
  when encrypted (uncompressed blocks too); the directory index is encrypted with the same key.
  Partitions `name.ucas`, `name_s1.ucas`, … (offset / `PartitionSize`).
- 4.27 globals hold LoaderInitialLoadMeta + LoaderGlobalNames + LoaderGlobalNameHashes (types 7–9),
  5.x globals `ScriptObjects`.
- A container is mounted through the `.pak` of the same name; the optional segment is `.o.utoc`.
- IoStore keeps no hash of the decrypted index (wrong keys are detected by strict index parsing).
- Loading (5.8): packages are found by `FPackageId::FromName(name)` (CityHash64 of the lowercase
  UTF-16 name) through the store; the directory-index path plays no part in loading, only in
  discovery by path (mod loaders, tools). A package's imports are resolved through its store
  entry's imported-package list (`AsyncLoading2.cpp` ~5579; the import map holds indices into it).
  Cooked 5.3+ packages also carry `ImportedPackageNames` (written unconditionally by
  `PackageStoreOptimizer`), which the runtime doesn't read.
- Bulk chunks: id = the owning package's id, index 0, kind BulkData (`.ubulk`), OptionalBulkData
  (`.uptnl`), MemoryMappedBulkData (`.m.ubulk`, 16 KiB aligned).

---

## 2. Architecture

### 2.1 Plugin (this repo, cdylib)
```
lib.rs          TC exports (W + ANSI stubs), panic guard on each export
read.rs         open .ucas/.utoc, global check, key lookup, ReadHeader, extract/test
archive.rs      listing: mount folders, files, $chunks, $legacy, info file
global.rs       global container lookup (§3.2)        info.rs     info file (§5)
legacy.rs       $legacy conversion session (§7)        profile.rs  game profile per container
pack.rs         packing: game index, plan, apply, undo, compact, new containers (§8)
add.rs          new files: ids, store entries, path↔name rules, question texts (§8.4)
validate.rs     ValidateOnReplace checks (§8.5)
pack_settings.rs  Configure dialog form ↔ [Pack] options (§8.8)
source_stream.rs  :uetoc.source stream (§9)
keys.rs, dialog.rs, busy.rs, config.rs, view.rs, log.rs, progress.rs, win.rs, ffi.rs (from uepak)
```
Build: `x86_64-pc-windows-gnu` (mingw) or MSVC (CI), LTO, stripped, ~2 MB.

### 2.2 Fork (`Civ01/retoc`, branch `tcl-iostore`; `master` untouched)
- New lean crate **`iostore`** (workspace member): TOC parse/serialize, partitions, positional
  streaming reads (`ChunkReader::next_piece`), AES, Zlib/Gzip/LZ4/Zstd/Oodle (`oodle_loader` from
  Civ01/repak, no download), hash checks, directory index, `GameProfile` + `Vanilla`; writer
  `edit::ContainerEdit` (open/create, replace, add, add_raw, remove, finish via temp + rename,
  rollback, `UndoRecord`), block writer, UE's perfect hash, directory-index writer. No panics on
  bad input (fuzz-style tests). TOC (119 TOCs), perfect hash (103), directory index (79) and
  container header (73) round trips are byte-identical to UE's.
- **`retoc`** crate, used for conversion: `ChunkDataSource` + `IoStoreContainer::open_with_source`
  (retoc parses, `iostore` reads the data), `backend_from_containers`,
  `build_legacy_without_bulk_data`, Gzip method, container-header edits (soft references kept),
  8-byte shader hashes, `EngineVersion::UE5_8`, and the legacy-header fixes of §7.3.
- Dependency: `iostore`/`retoc` = git, branch `tcl-iostore`. Local work: a `[patch]` in a
  `.cargo/config.toml` outside the repo (sandbox: it rewrites `Cargo.lock` on every build, so
  `git checkout Cargo.lock` before committing; CI builds `--locked`).

### 2.3 Game profiles
Every read goes through a `GameProfile` (magic, TOC preparation, decryption, compression names).
Only `Vanilla` exists; CUE4Parse ports land there (N4). Reader errors (`UnknownMagic`,
`BadDirectoryIndex`, `UnknownCompression`, `DecodeFailed{block}`) are detailed enough to decide
when to offer a profile.

---

## 3. Opening

### 3.1 Which file
TC associates `.utoc` and `.ucas` with the plugin.

| Opened | `[Options] OpenUtocAsVirtualFile=1` (default) | `=0` |
|---|---|---|
| `X.ucas` (or `X_s1.ucas`) | full listing (§4) | full listing |
| `X.utoc` | one file `<ContainerId>.txt` (§5; decimal `FIoContainerId`) | as `X.ucas` |

`.ucas` needs a same-named `.utoc` ("X.utoc not found next to X.ucas"). `CanYouHandleThisFile`:
`.utoc` by magic, `.ucas` when a same-named `.utoc` has it.

### 3.2 Global container (mandatory)
A container opens only with a valid global (the global counts as its own).
- **Lookup:** `global.utoc/.ucas` in the container's folder, then parents up to
  `GlobalParentLookupLimit` (default 5), never above the nearest folder named `Paks`.
- **Valid:** both files, the TOC parses, it has the script-objects chunk(s) (no key needed).
- **TOC version mismatch:** accepted, logged, one popup per (global, folder) per session.
- **Not found:** warning naming the folders searched and why the search stopped; archive not
  opened (`E_EABORTED`; TC adds its own "Error in packed file", kept). Cached 10 s per folder.
- The global is used by `$legacy` (§7) and packing (§8): the game = every container under the
  global's folder.

---

## 4. Listing (`.ucas`)
- Directory-index files with raw bytes as stored (Zen `.uasset/.umap`, `.ubulk`, `.uptnl`,
  `.m.ubulk`, `.ushaderbytecode`, …), as FModel shows them.
- Mount point as in uepak: `[Options] MountPoint=Folder|Root`, `MountFolderAttr`; same
  base/visible split (`../../../Moadra/Content/Mods/DebugInfo/` → folders `Moadra\Content\…`).
- Unpacked size = chunk length; packed = sum of its blocks; date = the `.utoc` time.
- Unsafe paths (`..`, absolute, `:`, empty parts) are skipped and logged.
- **`$chunks`** (hidden, `VirtualFolderAttr`): every chunk without a path as
  `<24 hex id>.<ChunkType>`, extractable raw.
- **`$legacy`** (hidden): §7.
- A key is asked on open only when the directory index is encrypted.

## 5. Info file (`<ContainerId>.txt`)
In memory, UTF-8 BOM, CRLF, fixed-width for a monospace font: sections `[Container]`, `[Header]`,
`[ContainerHeader]`, `[Signatures]`, `[Files]` (#, type, size, packed, compression, path) and
`[Chunks]` (#, id, partition, offset, blocks, flags, hash), the two tables linked by `#`, ASCII
only outside paths. Encrypted index without a key: paths read `(encrypted)`.

## 6. Extraction and testing
As uepak: one block at a time (decrypt, decompress), constant memory, progress + Cancel per
block, partial files deleted on failure. Errors: `E_ECREATE`, `E_EWRITE`, `E_BAD_DATA`
(decode/hash), `E_NOT_SUPPORTED` (missing Oodle, with a warning), `E_EABORTED` (key cancelled).
`PK_TEST` decodes every block and checks the chunk hash (all-zero hashes only decoded);
extraction logs a hash mismatch but keeps the file *(default; custom games may hash
differently)*. Block signatures are shown in the info file, not checked (rule unknown, §15).
Background unpack supported.

---

## 7. Legacy view (`$legacy`, N3)
Zen packages extracted as legacy cooked files, using retoc's `to-legacy` code through the fork's
data hook (one code path: partitions, AES, Oodle from `deps\`).

**Listing.** `$legacy` mirrors the container's tree with legacy names:
- each package → `X.uasset`/`.umap` + `X.uexp` (converted) and its `.ubulk`/`.uptnl`/`.m.ubulk`
  (the bulk chunks unchanged, streamed raw; a 3.5 GB chunk never sits in memory);
- each shader library → the rebuilt legacy library + `ShaderAssetInfo-*.assetinfo.json`;
- the global's `$legacy` also holds `scriptobjects.bin` (5.0+);
- other files as they are. Sizes of converted files are estimates until converted.

**Conversion.** A package is converted once per extract; its other files come from the same result.
Failure → `E_BAD_DATA` for its files, reason logged. Imports are resolved over every `.utoc` under
the global's folder (opened lazily, TOCs only, cached for the session; containers with another
container-header version or an unknown key are skipped, no prompts). Unresolvable imports become
retoc's `/Engine/UnknownPackage` placeholders (logged). Shader asset info is built from the store
entries of all loaded containers.

### 7.1 Engine version
`[Options] LegacyEngineVersion=Auto|UE4_27|UE5_0 … UE5_8`.
- `Auto`: from TOC + container-header versions (unique for 4.27, 5.0, 5.1/5.2 — told apart per
  package —, 5.3, 5.4, 5.5), narrowed for 5.6–5.8 by the shader hash size in the store entries
  (8 bytes ⇒ 5.8; 20 ⇒ 5.6/5.7) and, for versioned packages, by their custom versions (5.7 =
  UE5Release 61 / UE5Main 121 / FortniteMain 225; 5.8 = 68 / 123 / 268).
- Still ambiguous (unversioned 5.6/5.7/5.8 without shader maps): a prompt lists the versions that
  fit (ini value preselected, else the newest), with *Remember* (on; writes the ini) and *Don't ask
  again for this container* (until another container is opened). Cancel → `E_EABORTED`.
- A fixed ini value is used only if it fits the container's container-header version, else Auto.
- Packages with their own versions ignore all of this.

### 7.2 UE 5.8 specifics
- `FShaderHash` is 8 bytes (was 20) with no format version change: store entries, IoStore shader
  libraries (size detected from the buffers), legacy library version 3, asset info version 2.
  5.8 shader-group chunk ids are xxHash128 (only matters for to-zen, N2).
- UE 5.8 writes `FObjectImport::PackageName` in cooked packages too (ObjectName stands in for None);
  earlier engines only uncooked.
- Zen `_Unused` (was CookedHeaderSize): ≤5.6 legacy header size, 5.7.0–5.7.4 size − 8, 5.8
  size − 8 − 8 × imports. Harmless for `$legacy`; N2 must write it.

### 7.3 Differences from the engine's own cook
Measured on the 5.8 SML sample (engine cook of `BP_SML`, `BP_Monitor`, `M_dummy`, `T_dummy`) with an
independent header parser written from UE 5.8.4 source:
- **Same:** `.uexp` byte-identical; summary versions and custom versions, package flags,
  `PackageSource`, exports (sizes, flags, outers, templates, `bIsAsset`, script serialization
  offsets), every import except the classes below; shader libraries same content (code identical
  after decompression; `tools/shaderlib_compare.py`).
- **Engine struct/enum imports get class `Object`** (engine: `ScriptStruct`/`Enum`): Zen doesn't
  record it, the legacy loader finds native imports by class and `Object` always matches, while
  the name rule (`E` + uppercase = Enum, else ScriptStruct) is wrong for some engine enums
  (`TextureGroup`, `ACLCompressionLevel`). `LegacyGuessScriptTypes=1` guesses by name (better for
  viewers). Consequence: up to two names fewer (`Enum`, `ScriptStruct`), every later offset moves.
  Functions are exact (`Function` inside native classes and for `*__DelegateSignature`).
- **`SavedHash`** = BLAKE3-160 of our own `.uasset` + `.uexp` (UE's rule), so it differs whenever
  the header does. Only the editor copies it; nothing checks it.
- **Zen losses:** preload dependencies on script objects (BP_SML 205 → 127, BP_Monitor 37 → 33),
  name order, `NamesReferencedFromExportDataCount` over-counted (safe).
- Not investigated: `bNotAlwaysLoadedForEditorGame` (engine 1 on M_dummy/T_dummy, retoc 0).
- Results: byte-identical to retoc's CLI `to-legacy` on all samples, The Isle server (73k files),
  ARK pakchunk0+11 (328k files, ~1 min).

---

## 8. Packing (N1, N1b)
Input: raw Zen files as the plugin extracts them (directory-index files, `$chunks` files). TC
calls `PackFiles` once per F5/Alt+F5, so "per copy" = per call. Reference: UnrealReZen (patches
only packages the game has, reusing chunk id + store entry; writes an empty `.pak`) — this plugin
goes further with new files (§8.4).

### 8.1 What happens to each file
The game = every container under the global's folder. Each input is classified (`pack::plan`):

| Input | Result |
|---|---|
| a file the game has at the same path, in the target container | **replace**: chunk id, path, store entry kept; data replaced |
| a file the game has at the same path in another container | **copy**: that container's chunk id, path and store entry |
| a `$chunks` file | matched by chunk id (file name), as above |
| a package/bulk file the game doesn't have | **new file** (§8.4) |

Refused — whole copy cancelled, one message listing the files by reason, nothing written:
- files that belong in the companion `.pak` (`.ini`, `.locres`, `AssetRegistry.bin`, `.uplugin`, …);
- legacy cooked files (`.uasset` with the legacy magic, `.uexp`): N2;
- other new chunk files (shader libraries, …) and new files that can't be added (§8.4);
- anything into `$legacy`, into the global container, or `ContainerHeader` (always rebuilt).

Path of a dropped file: existing container = its mount point + the TC path (Folder view: the
visible folders are part of the TC path); new container = §8.3.

Delete (F8): directory-index and `$chunks` files (not `ContainerHeader`; folders as `*.*`); a
deleted package loses its store entry.

### 8.2 Changing an existing container (as uepak)
- New data appended to the last `.ucas` partition (a new `_sN` when it would exceed the
  partition size); `.utoc` rewritten via temp + rename.
- Undo record `<name>.utoc.uetoc-undo` (old `.utoc` + partition lengths) before each change,
  removed after success; rollback on error/cancel/panic; restore offered on the next open.
- `Compact=1` rebuilds after every change, `AutoCompactUnder=10M` rebuilds small containers
  (raw block copy into a new pair, swapped in via `.uetoc-old`).
- Free-space check, per-container busy guard + `.uetoc-lock` across processes.
- Encrypted: key from the read lookup, new blocks and index encrypted. Signed: confirmation, then
  written unsigned.
- **Mount point:** files outside it widen it to the deepest common folder (tcl-uepak §4.2.1); only
  the index is rewritten, existing files keep their game paths. Shown in the §8.4 question with an
  example (one existing entry before/after, where the game finds it, where added files go).

### 8.3 New container (Alt+F5 to a `.ucas` that doesn't exist)
- Format from the game: TOC version and container-header version of the global/others, block size
  most used, container id = CityHash64 of the lowercase name; unencrypted, unsigned. No `_P` check.
- **Mount point dialog** before every new container (`AskMountPoint=1`; 0 = the last one, no
  dialog): editable dropdown with the remembered mount points (`RecentMountPoints`, newest first,
  `MountPointHistory=10`, 0–50; saved on OK, not on Cancel), the game's folder of each dropped
  package (by the name inside it; `../../../` left out) and `Auto`. Preset: the last one, else the
  first suggestion, else Auto. A live preview shows where the first file goes and how many files
  the game has at those paths; text that isn't a mount point disables OK. Cancel writes nothing.
- **Paths** (`pack::new_container_full`): `Auto` = `../../../` + TC path, mount point = the files'
  common folder (UE's rule). A mount point M: M + TC path, unless the TC path already starts with
  M's folders (M without `../../../`, whole folders, case ignored) — then `../../../` + TC path.
  If both readings exist, the one the game has wins, else the full-path one. So a file dropped
  alone and the same file with its folders give byte-identical containers. Text only: no
  knowledge of project or plugin names.
- Refusals for a new container end with "Mount point for the copied files: …".
- **Companion `.pak`** (`WriteEmptyPak=1`): an empty pak v11 (347 bytes, own writer; read by repak;
  every reader accepts it for 4.27–5.8), only when none exists. Off: `WarnPakNeeded=1` warns.
- Copies of game files into a new container (patch containers) ask nothing beyond the dialog.

### 8.4 New files (N1b)
- **Package** (Zen `.uasset`/`.umap` the game doesn't have): chunk id = package id of the name
  **inside** it. Store entry: imported packages = its own `ImportedPackageNames`, no shader-map
  hashes, no soft references. On all 18 5.3+ sample games the rebuilt entry equals the cook's.
- **Bulk file** (`.ubulk`/`.uptnl`/`.m.ubulk`): matched to its package by name (in the copy, else
  in the game by path); id = package id, index 0, its kind; `.m.ubulk` written memory-mapped.
  It must come from the same cook as its package (laid out by the package's bulk-data map).
- **Refused:** new packages in pre-5.3 games; files that aren't Zen packages of this game; a
  package whose name the game already has elsewhere ("drop it there to replace it"); bulk files
  without their package.
- **One question per copy** when new files are added or a mount point must change ("Adding new
  files to an existing container may lead to unpredictable results." / "This new container gets
  files the game doesn't have …"): new files, `ValidateOnAdd` findings or moves, mount-point block.
  No writes nothing.

### 8.5 `ValidateOnReplace=Warn|Fix|Error|Off` (default Warn)
For every package that reuses a game store entry (replace and copy). A modded package can break
loading without any error at pack time (§1):

| Check | Versions | Fixable |
|---|---|---|
| package name inside vs the game's | all | no |
| imported packages vs the entry (added/removed/order) | 5.3+ | **yes**: entry gets the package's list, shader-map hashes kept |
| import map points past the entry's list | 5.0–5.2 | no |
| export count vs the entry; doesn't parse with the entry | ≤ 5.2 | no |
| public exports removed or class changed, with the game packages importing it | all (4.27: path hashes) | no |

4.27 import maps hold path hashes, so added imports can't be detected. Shader maps and bulk data
aren't checked. Warn = Yes/No list (`[fixable]` marks); Fix = correct, pack, then a report
(`[fixed]`/`[not fixable]`); Error = refuse the copy; Off = no checks. No false positives on
unmodified games (ARK 168,260, The Isle 36,574, G1R 842 packages, all samples).

### 8.6 `ValidateOnAdd=Warn|Fix|Error|Off` (default Warn)
For new packages:
- **Path vs name inside:** `<X>/Content/…` → `/Game/…`, `Engine/Content/…` → `/Engine/…`,
  `…/Plugins/…/<P>/Content/…` → `/<P>/…`. A mismatch loads (by name) but breaks discovery by
  path. Fix moves the file and its bulk files to the path the name gives.
- **Materials** (main export `Material`/`MaterialInstanceConstant`): packed without shader maps
  (no shader library is packed), reported, not fixable.
- Warn = findings in the question; Fix = moves listed in the question; Error = refuse; Off = none.

### 8.7 Compression and writer
Compression `Match` = the container's own method; for a new container the method most used by the
game's containers (None if uncompressed). Override: None/Zlib/Gzip (level 0–9)/LZ4/Oodle
(compressor + level). Zstd isn't offered (UE writes no Zstd containers; the 5.8 "Zstd" sample is
Zlib). A block is stored uncompressed when compression doesn't make it smaller. Chunk hashes,
perfect hash (v4+), directory index, partitions, meta flags as UE.

### 8.8 Configure dialog (`ConfigurePacker`, also Alt+F5 → Configure)
On add, Compression, Zlib/Gzip level, Oodle compressor + level (greyed when unused), On replace;
"Changing a container": Rebuild after every change, Rebuild containers smaller than (K/M/G);
"New container": empty `.pak`, Warn that a `.pak` is needed (only without the `.pak`), Ask for the
mount point, Remember N mount points. File locations at the bottom. Bad input → error line, dialog
stays open. OK writes `[Pack]` keeping comments in their column. `[Options]` are ini-only.

---

## 9. Source stream (N7)
On extraction an NTFS stream `<file>:uetoc.source` (format `uetoc-source 1`: legacy flag,
container, paks folder, entry, chunk, TOC hash, engine/engine range for `$legacy`) lets
tcl-uassetvw resolve names without asking. `[Options] SourceStream=View` (default: packages TC
extracts to view — `%TEMP%\_tc*\` — or drags out — `%TEMP%\CMD*.tmp`), `All` (every extracted file,
any operation), `Off`. Written only on volumes reporting `FILE_NAMED_STREAMS` (Wine's C: doesn't,
and would create a stray file), at most 4 KiB, main file times restored. TC's text search
(`WCS*.tmp`) is ignored. Spec and as-built: [`proposals/N7-source-stream.md`](proposals/N7-source-stream.md).

## 10. AES keys
`uetoc.keys.json` in the settings folder, same format, lookup order (folder/parent → same non-zero
GUID → dialog), dialog with "session only" and changed-key wording as uepak §3.4. A key is accepted
when the decrypted directory index parses cleanly; data-only cases: the first block decides. A
cancelled prompt covers the folder + GUID for 10 s (Find Files asks once per folder).

## 11. Oodle
`deps\oo2core_N_win64.dll` next to the plugin, any N, highest wins, picked up without restarting
TC (uepak's rules and code). Missing → one warning naming `deps\`.

## 12. Configuration: `uetoc.ini`
Plugin folder if writable, else the `wincmd.ini` folder. The shipped file documents each key.
```ini
[Options]
OpenUtocAsVirtualFile=1      ; .utoc shows <ContainerId>.txt; 0 = open like the .ucas
GlobalParentLookupLimit=5    ; parent folders searched for global.utoc (never above Paks)
MountPoint=Folder            ; Folder | Root
MountFolderAttr=Hidden
VirtualFolderAttr=Hidden     ; $chunks and $legacy
LegacyEngineVersion=Auto     ; Auto | UE4_27 | UE5_0 … UE5_8 (§7.1)
LegacyGuessScriptTypes=0     ; 1 = ScriptStruct/Enum by name (§7.3)
SourceStream=View            ; View | All | Off (§9)
DebugLog=0                   ; uetoc.log (or in memory, copied on errors)

[Pack]
ValidateOnAdd=Warn           ; Warn | Fix | Error | Off (§8.6)
Compression=Match            ; Match | None | Zlib | Gzip | LZ4 | Oodle
ZlibLevel=6
OodleCompressor=Kraken       ; Kraken | Mermaid | Selkie | Leviathan
OodleLevel=Normal            ; SuperFast … Optimal5
WriteEmptyPak=1
WarnPakNeeded=1
Compact=0
AutoCompactUnder=10M
AskMountPoint=1              ; §8.3
MountPointHistory=10
RecentMountPoints=           ; written by the plugin
ValidateOnReplace=Warn       ; Warn | Fix | Error | Off (§8.5)
```
Removed keys are ignored when read: `PackMode` (N1b), `NewMountPoint` (a non-Auto value becomes the
oldest remembered mount point; the line stays in old ini files and can be deleted).

## 13. Diagnostics and robustness
From uepak: panic guard on every export, `DebugLog=1` → `uetoc.log` or in-memory log copied to the
clipboard on errors, background unpack/pack with per-handle state, one key dialog at a time,
owner-less popups in background threads. TC needs the ANSI `PackFiles`/`DeleteFiles` exports even
with the W ones (else "Error packing files!" and no call). Caps 223 + `BACKGROUND_PACK`. A refusal
returns `E_EABORTED`, so TC adds "User abort!" after the plugin's message (as uepak).

---

## 14. Testing
`cargo test --release`, 49 tests. Samples in `IOSTORE_SAMPLES` (a folder of sample sets; sets that
are shader-only break the read tests), Oodle library folder in `IOSTORE_OODLE`, keys from `key.txt`
next to a container. Without samples the sample tests skip (CI). Windows CI checks real NTFS
streams.
- **Reading:** every sample lists every chunk once, passes `PK_TEST`, extracts at the listed size;
  info view; mount-point views; `$chunks`; global lookup rules; key dialog flow; damaged block.
- **`$legacy`:** byte comparison with retoc's CLI output (`RETOC_LEGACY_REF`), engine-version prompt
  (`IOSTORE_UNVERSIONED_56`).
- **Packing:** replace every file with itself; copy from another container + new container (repak
  reads the pak); refusals leave the container untouched; interrupted change restored; empty pak;
  `ValidateOnReplace` self-check on whole games (`VALIDATE_SELF`) and every mode; new packages on
  every 5.3+ sample (entry = the cook's), `ValidateOnAdd` modes, mount-point widening, bulk files
  (synthetic: no 5.3+ sample has bulk); mount-point rule and dialog on 35 sample games; Configure
  form round trips through the shipped ini.
- **Samples:** project files `IOStore_UEVanilla_test_containers_v3.7z`, `…58_COMPRESSED.7z`,
  `…58_SHADERBYTECODE_v2.7z`, `5.8_samples_full_same_asset.7z`, `IOStore_GameSpecific_test_containers_v1.7z`
  (G1R), `…_UE50_v2.7z` (trimmed with `tools/ucas_trim.py`). Large real containers via anonymous
  Steam, never uploaded: The Isle server (`tools/fetch_isle_server.sh`, ~1 GB, encrypted + signed)
  and ARK server (`tools/fetch_ark_server.sh`, ~3.5 GB, a 3.5 GB chunk).
- Performance is checked only when listing/reading code changes in a way that could matter:
  ~1 GB/s read + Oodle + hash, 81 MB peak on the 3.5 GB chunk.
- **TC checks** under Wine (TC 11.58.1 portable, WineHQ 11, Xvfb) cover only what a change touches;
  a full pass only on request. The user checks on real Windows.

### 14.1 Observed in TC
- Listing, F5, Test archives, Lister on the info file, key dialog, Oodle warning, 3.5 GB chunk
  with progress/Cancel/Background, Find Files: as specified.
- TC adds "Error in packed file" after any failed `OpenArchive` (all codes tried), not during
  Find Files.
- F3 extracts to `%TEMP%\_tc\name`, Find Files with text to `%TEMP%\WCSxxxx.tmp`, drag-out to
  `%TEMP%\CMDxxxx.tmp` (twice per drag, Windows). Ctrl+C in an archive does nothing; right-click
  Copy is TC's F5 dialog.
- Alt+F5 target: keep the `ucas:` prefix when editing the name (typing over it picks ZIP).
- Wine's dialog font draws ⚠ and → as boxes (fine on Windows). Lister wraps at ~75 characters.
- Packing checked: replace + auto-compact (hashes identical), refusals, new patch container with
  `.pak`, F8, Configure dialog, ValidateOnReplace prompt/report, N1b question with mount block,
  mount-point dialog (file alone and with folders give byte-identical containers).

---

## 15. Remaining work

**N2 — packing legacy cooked files** (next). Legacy `.uasset` + `.uexp` (+ bulk) converted to Zen
on pack with retoc's `to-zen`, then handed to the N1/N1b paths (replace, copy, new file). Pieces:
- detect legacy input (package magic, `.uexp` pairing; bulk files follow their package);
- target engine version from the game, as for `$legacy` (§7.1), prompt when ambiguous;
- script objects from the global (mandatory already), imports resolved against the game;
- write the per-version Zen `_Unused` value (§7.2) and the exact 5.8 import layout;
- store entry from to-zen (imports and, with shader libraries, shader-map hashes) — compared with
  the game's entry by `ValidateOnReplace` for replacements;
- optional: legacy shader libraries (`.ushaderbytecode`) → IoStore libraries, which would give new
  materials their shader maps; needs 5.8 shader-group ids (xxHash128 over 8-byte hashes);
- tests: round trip `$legacy` → pack → game-identical chunks on every sample, the engine's own
  legacy cook (SHADERBYTECODE and 5.8 SML samples) as input.

**Other**
- N4 custom game profiles (CUE4Parse ports) — needs the user's game samples.
- N5 TOC v9/v10 — when a released engine writes them.
- N6 tcl-uepak follow-ups: Find Files key-prompt grace; the 5.8 sample `.pak` labels gzip streams
  as Zlib (repak left them compressed).
- Real Windows checks: dialog layout and fonts, install from the CI package and the `.utoc`
  association (pluginst.inf names only `.ucas`), `dir /r` on a dragged-out package (§9).
- Block signature rule (The Isle `Signed`); on-demand containers if a sample turns up.
- Sample gaps: real bulk data in a 5.3+ sample, encrypted with a non-zero key GUID, optional
  segment, multiple partitions, localized packages, a real Zstd container.
