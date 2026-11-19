# tcl-iostore — Specification

Total Commander 11+ packer plugin (`uetoc.wcx64`) for Unreal Engine IoStore containers
(`.utoc` + `.ucas`). Sister of [tcl-uepak](https://github.com/Civ01/tcl-uepak) (`uepak.wcx64`,
legacy `.pak`), whose skeleton, conventions and lessons it reuses.

Spec version: v3 (2026-11-19): phase 1 (reading/extraction) done; N3 (legacy view, §6a) in work, then N1. Marks: **[decided]** = user decision,
**[proposed]** = my default, change it if you disagree, **[open]** = needs input or samples.

---

## 0. Status and roadmap

Phase 1 is implemented and checked in TC 11.58 under Wine (2026-11-19, §12a); a short check on
real Windows is open (§13).

### Phase 1: reading and extraction (done)
| # | Milestone | Where | Status |
|---|---|---|---|
| R1 | Container reader: partitions, block-by-block streaming, AES, typed errors, no panics, Oodle from Civ01/repak | `iostore` crate in `Civ01/retoc` branch `tcl-iostore` | ✅ |
| R2 | Plugin skeleton (from tcl-uepak), open `.ucas`/`.utoc`, global lookup, listing, `$chunks` folder, `.utoc` info file | plugin | ✅ |
| R3 | Extract and test (hash checks), AES keys (`uetoc.keys.json`, key dialog), Oodle `deps\` | plugin | ✅ |
| R4 | Game-profile hook (vanilla profile only; see §2.2) wired through every read path | both | ✅ |

### Next goals (candidates, to be ordered with the user)
| # | Goal | Scope | Size | Notes |
|---|---|---|---|---|
| N1 | **Packing, raw** | Raw Zen files into containers: replace/delete in existing containers, new patch containers from game files; writer in `iostore` | large | **Done 2026-11-19 (§6b)**, Configure dialog 2026-11-20. **N1b (highest backburner): replace with addition** (new packages) |
| N2 | **Packing, legacy input** | Legacy `.uasset/.uexp/.ubulk` converted to Zen on pack (retoc `to-zen`), auto-detected, else asked once per copy session **[decided]** | large | Needs the global's script objects (already mandatory) and an engine version for the target; Zen header-size field per version (§6a.2) |
| N3 | **Legacy conversion on extract** | Hidden `$legacy` folder: Zen packages as legacy `.uasset/.uexp/.ubulk…`, rebuilt shader libraries, `scriptobjects.bin` (retoc `to-legacy`) | medium | Done, incl. N3b (UE 5.8 shader hashes, §6a.1) |
| N4 | **Custom game support** | Game profiles ported from CUE4Parse (keys, custom encryption, block layouts, magics); per-folder profile, offered on a read failure **[decided]** | medium per game | Hook in place (§2.2); needs the user's game-specific samples |
| N5 | **TOC v9/v10** | `AddedSourceHashes`, `ContainerEncryptionMethod` (AES-CTR) | small | When a released engine writes them; CUE4Parse reads them |
| N6 | **uepak follow-ups** | Port the per-folder key-prompt grace (Find Files) to tcl-uepak; recheck its dialogs in TC under Wine | small | |

Order **[decided 2026-11-19]**: **N3** (part of reading) → **N1** (packing raw Zen files) → N2;
N4/N5 when samples or a new engine appear; N6 when convenient. N1 starts with its own spec round.

### Non-goals
- Packing anything that isn't already in the game (until N1b) or isn't a raw Zen file (N2).
- UE 4.25/4.26 containers (TOC v1/v2): may happen to work, not tested or promised.
- On-demand containers (`OnDemand` flag, `.uondemandtoc`): listed if possible, data read
  refused with a clear message **[proposed]**.
- Showing the companion `.pak` contents (use uepak for that) **[decided]**.

---

## 1. Formats (verified)

Checked against UE 5.8.4 source (`Core/Internal/IO/IoStore.h`, `Core/Private/IO/IoStore.cpp`,
`IoStoreUtilities/Private/IoStoreWriter.cpp`, `PakFile/Private/IPlatformFilePak.cpp`) and the
project's test containers.

| TOC version | Name | Engine | Test samples |
|---|---|---|---|
| 3 | PartitionSize | 4.27 | vanilla 4.27 (Oodle), encrypted 4.27 (Oodle; base64 key) |
| 4 | PerfectHash | (early 5.0 previews) | none |
| 5 | PerfectHashWithOverflow | 5.0–5.3 | VanySlash demo 5.0 (trimmed, 9157 chunks, Oodle); vanilla 5.1, 5.2, 5.3 (uncompressed) |
| 6 | OnDemandMetaData | 5.4 | vanilla 5.4, 5.4 AngelScript fork, G1R (Oodle) |
| 7 | RemovedOnDemandMetaData | (unreleased) | none |
| 8 | ReplaceIoChunkHashWithIoHash | 5.5–5.8 | vanilla 5.5–5.8; 5.8 compressed with Zlib, Gzip, LZ4; TheIsle 5.6 (encrypted, signed, Oodle) |
| 9, 10 | AddedSourceHashes, ContainerEncryptionMethod | after 5.8.4 | none; refused with "TOC version N is newer than this plugin supports" |

- Header 0x90 bytes, magic `-==--==--==--==-`. A header-size or block-entry-size mismatch is an
  error, not an assert.
- Container header versions: −1 PreInitial … 5 SoftPackageReferencesOffset (5.6+).
- **Chunk hashes** (`FIoStoreTocEntryMeta`), measured on the test containers: **SHA-1** of the
  uncompressed chunk in 4.27; **BLAKE3 truncated to 20 bytes** in 5.0–5.8 (v5, v6, v8).
- Blocks: each block's disk size is padded to 16 bytes with `buf[(i - size) % size]` (same rule
  as pak); AES-256-ECB over the padded block when encrypted; the directory index is encrypted
  with the same key. Blocks stored without compression are encrypted too (verified on the
  encrypted 4.27 sample). An offset in the uncompressed address space / block size = block index; a
  block offset / `PartitionSize` = partition index (`name.ucas`, `name_s1.ucas`, …).
- A container is mounted through the `.pak` of the same name; the optional segment is `.o.utoc`.

---

## 2. Architecture

### 2.1 Layers
```
uetoc.wcx64 (cdylib, this repo)
 ├─ lib.rs       TC exports (W + ANSI), panic guard on each export (from uepak)
 ├─ read.rs      .ucas/.utoc resolution, global check, key lookup, ReadHeader, extract/test
 ├─ archive.rs   listing model: mount folders, files, $chunks folder, info file
 ├─ global.rs    global container lookup (§3.2)
 ├─ info.rs      info-file text renderer (§5)
 ├─ profile.rs   game profile per container (§2.2)
 ├─ keys.rs, dialog.rs   uetoc.keys.json + key dialog (from uepak)
 └─ config.rs, view.rs, log.rs, progress.rs, win.rs, ffi.rs  (from uepak)
iostore crate (Civ01/retoc, branch tcl-iostore, workspace member `iostore/`)
 └─ container layer: TOC parse, partitions, streaming block reader, AES, decompression,
    hash checks, directory index, container header summary, GameProfile
```
**Deviation from v0 (decided while implementing):** retoc's library uses `tracing`/`serde`
throughout, so instead of feature-gating it the container layer is a new lean crate `iostore`
in the fork's workspace (deps: aes, sha1, blake3, flate2, lz4_flex, zstd, thiserror,
oodle_loader). retoc's own code is untouched and can move onto `iostore` later (writer,
conversion). Dependency: `iostore = { git = "https://github.com/Civ01/retoc", branch =
"tcl-iostore" }`. For local work against a checkout, a `[patch]` in a `.cargo/config.toml`
outside the repo. Build: `x86_64-pc-windows-gnu` (mingw cross-build) or MSVC (CI), cdylib only
(LTO; see the uepak gotcha), stripped: 0.84 MB.

### 2.2 Game profiles (hook for later custom-game support) **[proposed]**
Every byte read from a `.utoc`/`.ucas` goes through a `GameProfile` the reader is opened with:
- TOC header (magic, extra fields), TOC body decryption, chunk offset decoding,
  block entry layout, block decryption, compression-method name mapping, directory-index
  decryption, path fix-ups.
- Phase 1 ships only `Vanilla`. The trait is in the fork (`retoc::profile`) so CUE4Parse ports
  land in one place.
- Selecting a profile later: stored per folder like keys (`uetoc.profiles.json`), offered in a
  prompt when a vanilla read fails. Errors from the reader carry enough detail
  (`UnknownMagic`, `BadDirectoryIndex`, `UnknownCompression(name)`, `DecodeFailed{block}`) to
  decide when to prompt.

---

## 3. Opening

### 3.1 Which file **[decided]**
TC associates both `.utoc` and `.ucas` with `uetoc.wcx64`.

| Opened file | `[Options] OpenUtocAsVirtualFile=1` (default) | `=0` |
|---|---|---|
| `X.ucas` | full listing (§4) | full listing (§4) |
| `X.utoc` | one file: `<ContainerId>.txt` (§5) | same as opening `X.ucas` |
| `X_s1.ucas` (partition) | as opening `X.ucas` **[proposed]** | same |

- `X.ucas` needs `X.utoc` in the same folder; missing → "X.utoc not found next to X.ucas".
- `<ContainerId>` = the header's `FIoContainerId` as an unsigned decimal (e.g.
  `10437857082156768155`).
- `CanYouHandleThisFile`: `.utoc` by magic; `.ucas` has no magic, so: a same-named `.utoc` with
  valid magic exists **[proposed]**.

### 3.2 Global container (mandatory) **[decided]**
A container opens only when a valid global container is found (the global itself counts as
its own global).

**Lookup:** `global.utoc` + `global.ucas` in the container's folder, then each parent folder,
up to `[Options] GlobalParentLookupLimit` parent levels (default **5** [proposed]). If the path
contains a folder named `Paks` (case-insensitive, nearest one going up), the search never goes
above it: `…\Paks\~mods\Sub\X.ucas` checks `Sub`, `~mods`, `Paks` and stops. Without a `Paks`
folder in the path it walks up to the limit.

**Valid:** both files exist, the TOC parses, and it contains the script-objects chunk
(`ScriptObjects` in 5.x; `LoaderInitialLoadMeta` + `LoaderGlobalNames` in 4.27, whose global
holds types 7–9: InitialLoadMeta, GlobalNames, GlobalNameHashes). The check needs no key
(chunk ids aren't encrypted).

**Version mismatch** (global TOC version ≠ container TOC version): accepted with a warning
**[decided]**. The warning is logged every time and shown as a popup once per
(global, container folder) per TC session **[proposed]**, so Find Files over a folder doesn't
show one per container.

**Not found:** warning naming the folders searched and why the search stopped (limit or `Paks`
boundary), then the archive isn't opened (`E_EABORTED`) **[decided]**. TC then adds its own
"Error in packed file" (it does for any failed open, §12a); keeping both messages
**[decided 2026-11-19]**. Cached per folder
for 10 s so TC's repeated opens don't repeat the popup **[proposed]**.

In phase 1 the global is only validated and named in the info file. It's needed later for
conversion and Zen-aware packing.

---

## 4. Listing (`.ucas`)

- Entries: the directory index, mount point included, raw bytes as stored (Zen `.uasset/.umap`,
  `.ubulk`, `.uptnl`, `.m.ubulk`, `.ushaderbytecode`, …), as FModel/CUE4Parse show them
  **[decided]**.
- View modes: same as uepak (`[Options] MountPoint=Folder|Root`, `MountFolderAttr`), same
  base/visible split of the mount point (e.g. `../../../Moadra/Content/Mods/DebugInfo/`)
  **[proposed]**.
- Unpacked size: the chunk's length. Packed size: the sum of its blocks' compressed sizes (blocks
  are never shared between chunks). Date: the `.utoc` modification time.
- Path safety: entries containing `..`, absolute paths, `:` or empty components are skipped and
  logged (as uepak).
- **Unnamed chunks** **[decided]**: every chunk not referenced by the directory index is listed in
  a hidden folder `$chunks` at the panel root (attribute from `[Options] VirtualFolderAttr`,
  default `Hidden`) as `<24 hex chunk id>.<ChunkType>`, e.g.
  `b53fffed16aaad1300000006.ContainerHeader`. Extracting gives the raw chunk bytes.
- No AES key needed unless the directory index is encrypted (`Encrypted` flag); then the key
  is asked on open.

## 5. Info file (`<ContainerId>.txt`) **[decided]**
Generated in memory on open; UTF-8 with BOM, CRLF; listed with its real size and the `.utoc`
date; extract/F3 gives the text.

**Layout: fixed-width, for a monospace font** (Lister's default) **[decided 2026-11-19]**,
replacing the earlier tab-separated layout:
- Sections `[Container]`, `[Header]`, `[ContainerHeader]`, `[Signatures]`, `[Files]`.
- Key/value lines: keys padded to one column width per file (`Key ......... value` style
  without dots: `Key` + spaces), values start in the same column in every section.
- Two tables, each with a header row + a separator row of `-`, columns padded with spaces to
  the widest value, numbers right-aligned (with `,` thousands separators), text left-aligned.
  Same rows in both (directory-index order, then unnamed chunks), linked by `#` (TOC entry
  index). Two narrow tables instead of one ~170-character table, because Lister wraps long
  lines by default:
  - `[Files]`: #, type, size, packed, compression, path (last, so long paths push nothing).
  - `[Chunks]`: #, chunk id, partition, offset in the data file, blocks, flags, hash.
- Only ASCII outside paths (no box-drawing characters), so any font/codepage lines up.
- If the directory index is encrypted and no key is available, paths read `(encrypted)` and
  the file still opens.

## 6. Extraction and testing

As uepak §3.3:
- Streaming one block at a time (decrypt the 16-aligned block, decompress), constant memory,
  progress and Cancel per block; partial files deleted on failure.
- Errors: create → `E_ECREATE`, write → `E_EWRITE`, decode/hash → `E_BAD_DATA`, missing Oodle →
  warning + `E_NOT_SUPPORTED`, key prompt cancelled → `E_EABORTED`.
- `PK_TEST`: decode every block and check the chunk hash (SHA-1 for TOC v3, BLAKE3-160 for v5+;
  a chunk whose hash is all zero is only decoded); a mismatch → `E_BAD_DATA`. Extraction
  computes the same hash but only logs a mismatch and keeps the file (a custom game may hash
  differently) **[proposed]**. Block signatures (`Signed`) are reported in the
  info file but not checked **[proposed]**: retoc notes they don't match a plain SHA-1 of the
  stored bytes; to be investigated with TheIsle.

## 6a. Legacy view (`$legacy`, N3) **[decided 2026-11-19]**
Extracting Zen packages as legacy cooked files (what a `.pak` holds), using retoc's `to-legacy`
code (`asset_conversion::build_legacy`, `shader_library::rebuild_shader_library_from_io_store`).

**Listing.** A hidden folder `$legacy` at the panel root (attribute `VirtualFolderAttr`, like
`$chunks`) mirrors the container's file tree, mount point included, with legacy names:
- each package (`.uasset`/`.umap` with an `ExportBundleData` chunk) → `X.uasset` (or `.umap`) +
  `X.uexp` (converted), followed by its `X.ubulk` / `X.uptnl` / `X.m.ubulk`: those are the bulk
  chunks' bytes unchanged (as retoc writes them), so they are streamed like raw files, with real
  sizes and no memory cost (a 3.5 GB bulk chunk never sits in memory);
- each shader library (`ShaderArchive-*.ushaderbytecode`) → the rebuilt legacy library + its
  `ShaderAssetInfo-*.assetinfo.json`;
- the global container's `$legacy` also holds `scriptobjects.bin` (UE 5.0+ only; retoc writes it
  for `to-zen` round trips);
- every other file (`.ini`, `.locres`, `AssetRegistry.bin`, …) as is.
Raw files stay where they are; `$legacy` is extra. Sizes of converted files in the listing are
estimates (the Zen package size for `.uasset`, 0 for `.uexp` and the asset-info JSON) **[proposed]**:
the real size is only known after conversion.

**Conversion.** On extract/test of a `$legacy` file the package is converted in memory once; its
other files (`.uexp`, bulk) come from the same result (listed next to each other, so TC asks for
them in a row). A failed conversion → `E_BAD_DATA` for that package's files, reason in the log.

**Imports from other containers** **[decided]**: on the first legacy extraction the plugin opens
every `.utoc` in the global's folder and its subfolders (lazily, cached for the TC session; TOCs
only, data read on demand) and resolves imports across all of them. Containers with another TOC or
container-header version, or encrypted with no known key (keys file or already entered), are
skipped with a log line; no key prompts for them **[proposed]**. Imports that still can't be
resolved become retoc's `/Engine/UnknownPackage` placeholders and are logged **[proposed]**.
Shader asset info is built as retoc does, over all loaded containers: package store entries
say which packages use the library, and only those packages are read.

**Engine version** **[decided]**: `[Options] LegacyEngineVersion=Auto` (or `UE4_27`, `UE5_0` …
`UE5_8`). 5.7 and 5.8 share the object version (1018) but **not the legacy layout**: UE 5.8 writes
`PackageName` into the imports of cooked packages too (ObjectName stands in for None), earlier
engines only uncooked (found 2026-11-19 via tcl-uassetvw; retoc never wrote it — fixed in the fork,
195bf45). How 5.8 is told apart: 8-byte shader hashes in the store entries (§6a.1); for versioned
packages their custom versions (FUE5ReleaseStream/UE5MainStream/FortniteMainBranch above the 5.7
values 61/121/225, measured on the samples); else the prompt offers 5.6 / 5.7 / 5.8 separately.
- `Auto`: derived from the TOC + container-header versions (retoc's heuristics, 5.1/5.2 told
  apart per package by bulk data). Unique for 4.27, 5.0, 5.1/5.2, 5.3, 5.4, 5.5.
- The heuristic **fails** when it gives no version (unknown combination) or several (container
  header `SoftPackageReferencesOffset` = 5.6, 5.7 and 5.8; 5.7+ adds import type hierarchies to
  the legacy header). Then a prompt lists the versions that fit, the ini value preselected
  (with `Auto`: the newest candidate) **[proposed]**, and two checkboxes:
  - *Remember (save to `uetoc.ini`)*, on by default: writes `LegacyEngineVersion`;
  - *Don't ask again for this container*: the pick holds until a different container is
    opened (any TC operation in between keeps it).
  Cancel aborts the extraction (`E_EABORTED`).
- A fixed ini value is used only if it fits the container (same container-header version as
  that engine writes); otherwise it is ignored for that container (log line) and `Auto` applies,
  so a remembered 5.7 never breaks a 4.27 game **[proposed]**.
- Packages that carry their own versions (uncooked/versioned) ignore all of this.

**Results (2026-11-19).** Byte-identical to retoc's own `to-legacy` (CLI, same folder) for every
file: all vanilla samples 4.27–5.8 incl. encrypted 4.27 (490 files), G1R (1,685), compressed 5.8
zlib/gzip/lz4 (220), The Isle server pakchunk2 (73,149, unversioned 5.6, prompt answered 5.6),
ARK server pakchunk0+11 (328,189, unversioned, prompt answered 5.6) — ARK's 328k files convert in
~1 min, store setup (3 containers, 19 MB of TOCs) well under a second. Versioned samples never
prompt. Shader libraries: see §6a.1.

### 6a.1 UE 5.8 shader hashes (N3b) **[found 2026-11-19]**
Sample `IOStore_UEVanilla58_test_containers_SHADERBYTECODE.7z` (project Moadra, SM5, one dummy
material + texture): the same cook packaged with IoStore (`iostore\pakchunk0`) and as a plain
`.pak` (`pak\pakchunk0/2/11`). The `.pak` holds the engine's own legacy files for the same
content (`M_dummy`, `T_dummy` `.uasset/.uexp`, `ShaderArchive-Global/Moadra_Chunk0-PCD3D_SM5-…
.ushaderbytecode`), so it is a reference that doesn't depend on retoc.

UE 5.8 shortened `FShaderHash` from 20 bytes (SHA-1) to **8 bytes**, with no container-header or
IoStore-library version change (CUE4Parse keys it on `GAME_UE5_8`). It affects:
- container header store entries (`ShaderMapHashes`): retoc reads 20-byte hashes and fails
  ("Package metadata will be unavailable"), so **every package of a 5.8 container whose
  materials have shader maps fails to convert**, not only the libraries (the earlier 5.8 samples
  had no shader maps, so they passed);
- the IoStore shader library (`ShaderMapHashes`, `ShaderHashes`; library version still 1) and the
  shader-group chunk ids derived from hashes;
- the legacy library (`FSerializedShaderArchive`): version **3** in 5.8 (retoc writes/expects 2),
  8-byte hashes;
- the shader asset info JSON (hash strings).

**Done (2026-11-19)** — plan as below, all steps carried out:
- retoc fork 7346193: `FShaderHash` (8 or 20 bytes) everywhere shader hashes live; size detected
  from the store-entry buffer (exact) and from the IoStore library header (the size that parses
  the chunk exactly); legacy library v3 for 8-byte hashes; asset info `AssetInfoVersion: 2`;
  `EngineVersion::UE5_8`; to-zen of v3 libraries refuses for now (5.8 shader group ids are
  xxHash128 over the hashes, N2).
- Plugin: `UE5_8` in the ini/prompt. Candidates are narrowed by the shader hash size found in the
  loaded containers' store entries: 8 bytes ⇒ 5.8, no prompt; 20 bytes ⇒ 5.6 or 5.7; none (e.g.
  server builds) ⇒ versioned packages decide by custom versions, unversioned ones ask 5.6/5.7/5.8.
  (An earlier "UE 5.7 / 5.8" merged entry was wrong: see the import layout above.)
- Results on the sample: no prompt; shader libraries **same content as the engine's** (both the
  cook's `ShaderLibrarySource` and the `.pak`: 301 maps / 3,767 shaders and 4 / 36, code
  identical after decompression; `tools/shaderlib_compare.py`); `.uexp` byte-identical to the
  `.pak`; `.uasset` equivalent but not byte-identical. After the PackageName fix the import maps match the
  engine's except retoc's class name `Object` for imports of script functions/structs (engine:
  `Function`/`ScriptStruct`; script objects carry no class; functions fixed in §6a.2), plus the saved package hash, name
  hashes, package source CRC and fewer preload dependencies (BP_SML: 127 vs 205) — all versions,
  Zen drops or retoc approximates them; asset info lists only
  packages present in the containers (engine also lists plugin materials and global "None"
  entries). Earlier samples unchanged (byte-identical to the previous references).
- TC check (N3b only): F5 of `$legacy` from the sample, no prompt, log shows hash size 8;
  files identical to the sandbox run except the Global library (34 bytes smaller: the Windows
  Oodle DLL compresses a few shaders differently from the Linux build; same content).
- Side finding for uepak: the sample `.pak` says Zlib, but its entries are gzip streams
  (`1f 8b`); repak unpacked them still compressed. To check in tcl-uepak.

Plan:
1. Sample: add `global.utoc/.ucas` of the IoStore build (required; missing from the upload);
   optionally `Saved\Cooked\Windows\Moadra\Metadata\ShaderLibrarySource\ShaderAssetInfo-*.assetinfo.json`
   (not staged into the `.pak`) as reference for the asset info.
2. Fork (retoc): hash size as a parameter: detect it in the container header (entry buffer size
   is exact: 16·n + 8·imports + h·shader maps, h ∈ {8, 20}) and in the IoStore library (array sizes
   against chunk length); add `EngineVersion::UE5_8` (TOC v8, header v5, object version 1018,
   8-byte shader hashes, legacy library v3); read/write legacy library v3. Same change is needed
   later for `to-zen` (N2).
3. Plugin: `UE5_8` in `LegacyEngineVersion`/prompt; the version heuristic uses the hash size
   (8 bytes ⇒ 5.8), which also removes the 5.6/5.7-vs-5.8 prompt for any container with shader
   maps; 5.7 vs 5.8 without shader maps stays a prompt (identical otherwise).
4. Tests: `$legacy` of the IoStore sample against the `.pak` reference: packages byte-identical
   if the cook is deterministic (else field-wise), shader libraries compared by content (shader
   map/shader hashes, shader code bytes, entries; order and compression may differ), asset info
   by content. Plus all earlier samples unchanged (5.7-and-older 20-byte path).

**Integration.** retoc's conversion runs on its own container type (`IoStoreTrait`); the fork adds
a chunk-data hook so its reads go through the `iostore` crate (partitions, AES, Oodle from
`deps\`, one code path) and a backend built from a chosen list of containers. No `.pak` writing
(repak) is linked into the plugin for this.

### 6a.2 Review of the fork against UE 5.8 **[2026-11-19]**
Sample `5.8_samples_full_same_asset.7z` (SML mod, 16 packages; versioned = tagged properties and
unversioned containers, each with the engine's own cook of `BP_SML`), compared with an independent
header parser written from the UE 5.8.4 source. 5.8 changed no Zen/IoStore format version (TOC v8,
container header v5, Zen package v3, export map, script objects). `.uexp` byte-identical.

Fixed (retoc fork 66f361d, plugin uses it):
- **Script import classes** (all versions): retoc wrote `Object` for every script import that is
  not a package, class or CDO. Now `Function` inside a native class and for top-level
  `*__DelegateSignature`. Top-level structs/enums stay `Object` by default: Zen doesn't say which,
  the legacy loader finds native imports by class (`FindImportFast(class, outer, name)`), so a wrong
  guess fails the import while `Object` always matches, and the obvious name rule (`E` + uppercase =
  enum) is wrong for real engine enums (`ACLCompressionLevel`, `TextureGroup`, …) and structs
  (`FEQS…`). `[Options] LegacyGuessScriptTypes=1` turns the name guess on (better for viewers).
  BP_SML: imports match the engine except the 5 top-level structs; all match with the guess.
- **`bIsAsset`** (all versions): UE's rule (`UObject::IsAsset`): top-level, `RF_Public`, not CDO or
  transient, not a plain `UClass` (Blueprint/Verse classes override `UClass::IsAsset`). retoc also
  required `Standalone|Transactional`, so every Blueprint class asset had 0 (15 of 16 packages).

Also fixed on request although they don't affect loading (retoc fork 0717d17; UE 5.8.4 source):
- **`PackageSource`** = `FCrc::StrCrc_DEPRECATED(upper(file base name))` (2-byte chars), now written
  exactly (BP_SML, M_dummy, T_dummy match the engine). Not read by `FLinkerLoad`, the Zen loader or
  `UPackage`; shipping editors even write a random value.
- **`SavedHash`** = BLAKE3-160 of `.uasset` + `.uexp` without the 4-byte footer, field zeroed; now
  computed over our own output (valid, equals the engine's only where the header does). Only
  copied into `UPackage` in editor builds; not checked.
- **`ScriptSerializationStart/EndOffset`** (only packages with tagged properties, i.e. without
  `PKG_UnversionedProperties`; the Zen loader never sees them): retoc wrote `(0, SerialSize)`;
  `FLinkerLoad::MarkScriptSerializationStart/End` `checkf` them against the real positions, so such a
  file asserted when loaded through the legacy loader in builds with checks (editor,
  Debug/Development); Shipping compiles the check out. Now the real range: walk the tag stream
  (control byte, tags carrying their sizes; 5.4+ and 5.2/5.3 layouts) to its `None`; an export that
  doesn't parse keeps the old value. Matches the engine on BP_SML (class 0–38, CDO 0–130, functions
  0–9), M_dummy (0–99) and the 1 MB T_dummy (0–131).
- Seen while checking, not fixed: `bNotAlwaysLoadedForEditorGame` (engine 1 on M_dummy/T_dummy,
  retoc 0; editor-game flag, not found in `FLinkerLoad`; not investigated further).
- **Zen `_Unused` (was CookedHeaderSize)**: UE ≤5.6 stores the legacy header size; 5.7.0–5.7.4
  store size − 8; 5.8 size − 8 − 8 × imports (PackageStoreOptimizer.cpp, frozen to keep patches
  small). retoc treats it as the size: harmless for `$legacy` (padding only); N2 must write the
  version's formula.
- Inherent Zen losses: preload dependencies on script objects (BP_SML 205 → 127; retoc adds its own
  class/template entries), name order, NamesReferencedFromExportDataCount over-counted (safe).
- Backburner (retoc CLI): `--version UE5_8` help text ("same formats as 5.7"); a single `.utoc`
  without its global fails with "does not contain ScriptObjects" (pass the folder).

## 6b. Packing raw Zen files (N1) **[done 2026-11-19]**
Input is what the plugin extracts raw (directory-index files, `$chunks` files). Reference checked:
UnrealReZen (only patches packages that exist in the game: finds each file by path in the game's
containers, reuses its chunk ID and container-header store entry, skips the rest; writes a fixed
empty `.pak` next to its container).

**What may be packed** **[decided]**
- `PackMode=ReplaceOnly` (default; the only implemented mode): every file must exist in the game
  with the same path (mount point + path): in the target container, else in any container under the
  global's folder (the legacy view's store). Its chunk ID and, for packages, its store entry
  (imported packages, shader map hashes) are reused unchanged. The new data replaces the chunk.
- `PackMode=ReplaceWithAddition`: **dummy for now** (accepted in ini and dialog, behaves as
  ReplaceOnly, logged). Backburner **N1b, highest priority**: new packages, chunk ID from the
  package name, store entry derived (5.3+ imports from the Zen header; no shader maps, warning).
- **Refused, whole copy cancelled, one message listing the files** **[decided]**:
  - files the game doesn't have (ReplaceOnly);
  - files that live in the companion `.pak`, not in IoStore (`.ini`, `.locres`, `AssetRegistry.bin`,
    `.uplugin`, `.uproject`, …: anything that isn't a package, bulk file or shader library);
  - legacy cooked files (`.uasset` with the legacy magic, or `.uasset` + `.uexp`): N2;
  - anything into `$legacy` (read-only view) or into the global container **[proposed]**.
- `$chunks` files are matched by chunk ID (file name); `ContainerHeader` is never taken from input
  (always rebuilt) **[proposed]**.
- Delete (F8): directory-index files and `$chunks` files (not `ContainerHeader`); a deleted package
  loses its store entry **[proposed]**.

**Changing an existing container** **[decided: like uepak]**: new data appended to the `.ucas` (last
partition; a new `_sN` partition when the partition size would be exceeded), `.utoc` rewritten
(small); undo copy `<name>.utoc.uetoc-undo` with the old `.utoc` and the old partition lengths;
rollback on error/cancel/panic, restore offered on next open; `Compact=1` rebuilds into a new pair
(drops dead space), `AutoCompactUnder` (default 10 MB) always rebuilds small containers;
`UndoLimit`; free-space check; per-container busy guard + cross-process lock (as uepak).
- Encrypted containers: confirmation, key from the read lookup, new blocks and the rewritten index
  encrypted with it (uepak rule) **[proposed]**.
- **Signed containers** **[decided]**: confirmation that the game may reject an unsigned container;
  then written unsigned (flag and signature data removed).

**New container from scratch** (Alt+F5 to a `.ucas`/`.utoc` that doesn't exist)
- Format taken from the game (the global and the other containers in its folder): TOC version,
  container-header version, shader hash size; container ID = UE's (CityHash64 of the lowercase
  container name) **[proposed]**; mount point from the Configure dialog (default `../../../`), paths
  as TC gives them **[proposed]**; unencrypted, unsigned (uepak rule) **[proposed]**.
- **Companion `.pak`** **[decided]**: offered only here (checkbox in the pack settings, ini
  `WriteEmptyPak=1`): an empty `.pak` (empty index + footer, ~250 bytes, written by our own code, no
  repak) named like the container, if none exists. Pak version **11** for every engine 4.27–5.8
  (every reader accepts older pak versions; 11 = 4.26+). When off, `WarnPakNeeded=1` warns instead.
- Name: no `_P` check (user's business) **[decided]**.

**Compression** **[decided: match the game]**: the method of the container being changed; for a new
container, the method most used by the game's other containers (None when they're uncompressed);
override in the Configure dialog (None/Zlib/Gzip/LZ4/Oodle with levels, block size; Zstd not offered:
UE has no Zstd format, §6a.1 note). Store-if-not-smaller per block **[proposed]**.

**Writer** (`iostore` crate) **[proposed]**: TOC v3/5/6/8, chunk hashes (SHA-1 ≤ v3, BLAKE3/20
later), blocks with compression + AES-ECB padding as UE, perfect-hash chunk table (v4+; UE's
algorithm), directory index (encrypted when the container is), partitions, meta flags, TOC
signature data dropped. Container header (de)serialized with the fork's `retoc::container_header`.

**Configure dialog** (packing part) **[done 2026-11-20, see As built]**: compression (Match game / methods + level),
block size, PackMode (ReplaceOnly / ReplaceWithAddition — "later"), Compact, AutoCompactUnder,
UndoLimit, WriteEmptyPak, mount point for new containers, WarnPakNeeded.

**Tests** **[proposed]**: round trips on every sample (extract raw → replace all → container reads
back chunk-for-chunk identical; container header store entries unchanged); modify/undo/rollback/
compact as uepak; encrypted 4.27 and The Isle; the empty `.pak` read by repak; TC check of the new
pieces only; a game load test by the user (patch container + empty `.pak`).

**As built (2026-11-19)** — differences from the plan above:
- Writer: `iostore::edit::ContainerEdit` (open/create, replace, add, add_raw, remove, finish via
  temp + rename, rollback) + `write.rs` (blocks), `perfect_hash.rs`, `dirindex.rs`; container
  header edits in `retoc::container_header` (soft references kept). TOC round trip on 119
  sample TOCs, perfect hash on 103 and directory index on 79 are byte-identical to UE's.
- Undo: one `<name>.utoc.uetoc-undo` per container (old `.utoc` + partition lengths), written
  before each change and removed after success; restore offered on the next open. No
  `UndoLimit` (one record is all there is). New partitions are started when a block wouldn't fit
  the partition size.
- Encrypted containers: **no extra confirmation**; the key comes from the read lookup (prompt if
  unknown) and new blocks + the directory index are encrypted with it.
- Signed containers: confirmation, then written unsigned (as decided).
- New container: TOC version, block size, compression from the game's containers; mount point
  `NewMountPoint=Auto` = the common folder of the packed files (UE's rule), else the given one
  (files outside it refused). A replaced/added file outside an existing container's mount point
  moves the mount point up to the common folder. Empty v11 `.pak` = 347 bytes, read by repak.
- Refusals list every file by reason in one message (not in the game; companion-pak files; legacy
  cooked files → N2; read-only: `$legacy`, the global, `ContainerHeader`); nothing is changed.
- Delete accepts `*.*` (whole folders); an empty match is refused.
- Compact: raw block copy into a new pair, swapped in through `.uetoc-old`; also run by
  `AutoCompactUnder` (default 10M).
- TC: caps 223 (NEW | MODIFY | MULTIPLE | DELETE | OPTIONS | BY_CONTENT | SEARCHTEXT),
  `BACKGROUND_PACK`; **TC needs the ANSI `PackFiles`/`DeleteFiles` exports** even when the W
  ones exist (without them: "Error packing files!" and no call); stubs added as in uepak.
  Busy guard + `.uetoc-lock` from uepak.
- Configure dialog (2026-11-20, `ConfigurePacker`, also the Configure button of TC's Alt+F5
  dialog): Mode, Compression (Match the game / None / Zlib / Gzip / LZ4 / Oodle), Zlib/Gzip level,
  Oodle compressor + level (level lists greyed when the method doesn't use them; Match keeps
  both), Rebuild after every change, Rebuild containers smaller than (size with K/M/G), Write
  empty `.pak`, Warn that a `.pak` is needed (only when no `.pak` is written), Mount point (empty =
  Auto; backslashes and a missing trailing `/` are fixed). File locations at the bottom. Bad
  input shows a line in the dialog, which stays open. OK writes the `[Pack]` lines into the
  settings ini, keeping every comment in its column. Form logic in `pack_settings.rs` (tested).
  Block size and UndoLimit from the plan are not offered (block size comes from the game; one
  undo record).
- Not yet: ReplaceWithAddition (N1b), legacy input (N2).

## 7. AES keys **[decided]**: own file
`uetoc.keys.json` in the settings directory, same format, lookup order (folder/parent folder →
same non-zero GUID → dialog), dialog, "session only" checkbox and changed-key wording as uepak
§3.4. Wrong-key detection: IoStore stores no hash of the decrypted index, so a key is accepted
when the decrypted directory index parses cleanly (sane mount-point string, string table and
entry indexes in range) **[proposed]**; for data-only cases the first block's decode decides.

## 8. Oodle **[decided]**: own `deps\` only
`deps\oo2core_N_win64.dll` in the plugin folder, any N, highest wins, picked up without
restarting TC: the same rules and code as uepak (Civ01/repak `oodle_loader`).

## 9. Configuration: `uetoc.ini`
Location as uepak (plugin dir if writable, else the `wincmd.ini` folder).
```ini
[Options]
OpenUtocAsVirtualFile=1     ; .utoc shows <ContainerId>.txt; 0 = open like the .ucas
GlobalParentLookupLimit=5   ; parent levels searched for global.utoc (Paks boundary always applies)
MountPoint=Folder           ; Folder | Root
MountFolderAttr=Hidden
VirtualFolderAttr=Hidden    ; attribute of $chunks and $legacy
LegacyEngineVersion=Auto    ; Auto | UE4_27 | UE5_0 … UE5_8 (§6a)
LegacyGuessScriptTypes=0    ; 1 = ScriptStruct/Enum by name for top-level script imports (§6a.2)
[Pack] (N1, §6b)
PackMode=ReplaceOnly        ; ReplaceOnly | ReplaceWithAddition (later: N1b; acts as ReplaceOnly)
Compression=Match           ; Match | None | Zlib | Gzip | LZ4 | Oodle
ZlibLevel=6 / OodleCompressor=Kraken / OodleLevel=Normal
WriteEmptyPak=1             ; new containers: write an empty companion .pak
WarnPakNeeded=1             ; when WriteEmptyPak=0: warn that a .pak is needed
Compact=0 / AutoCompactUnder=10M / NewMountPoint=Auto
DebugLog=0
```
`ConfigurePacker` opens the packing settings dialog (§6b); `[Options]` stay ini-only.

## 10. Diagnostics and robustness
Taken from uepak: panic guard on every export, `DebugLog=1` → `uetoc.log` or in-memory log +
clipboard on error, background unpack (`GetBackgroundFlags = UNPACK`) with per-handle state,
one key dialog at a time, popups without an owner in background threads.

## 11. Fork changes (`Civ01/retoc`, branch `tcl-iostore`; `master` untouched) **[decided]**
All in the new `iostore` crate (§2.1) **[done]**:
1. Reader: every partition (`name_sN.ucas`; a missing one fails only when read), positional
   reads (thread-safe), `ChunkReader::next_piece` (one decoded block per call) and `Read`;
   header/entry-size/partition/block checks instead of asserts; typed errors (`thiserror`).
2. Dependencies: only the container layer (no clap/rayon/indicatif/jmap/tracing/serde).
3. Compression: Zlib, Gzip, LZ4, Zstd, Oodle (`oodle_loader` from Civ01/repak, no download).
4. Hash checks (SHA-1 for TOC ≤ 3, BLAKE3-160 for ≥ 4), `ContainerId`, `Guid` (displayed as
   the 16 bytes in file order, as tcl-uepak), chunk-type names for both numberings.
5. `GameProfile` trait (magic, TOC preparation, decryption, compression names) + `Vanilla`.
6. No panics on unexpected input: fuzz-style tests truncate and corrupt a TOC byte by byte
   (they found a 66 GB allocation from a corrupted partition count, fixed).
7. Tests: synthetic containers (all methods, versions 3/5/6/8, encryption, partitions,
   damage) and every chunk of the sample containers (`IOSTORE_SAMPLES`, `IOSTORE_OODLE`).
8. For the legacy view (N3), in the `retoc` crate: `ChunkDataSource` +
   `IoStoreContainer::open_with_source` (retoc parses the TOC, data comes from `iostore`),
   `backend_from_containers`, `build_legacy_without_bulk_data`, `FGuid::from_bytes`, a `Gzip`
   compression method (TOCs naming it failed to parse in retoc), workspace `oodle_loader`/`repak`
   from Civ01/repak without download (`retoc_cli` turns download back on). retoc's own
   `test_zen_asset_identity_conversion` fails on `master` too (test files missing).
9. UE 5.8 and legacy fixes in the `retoc` crate: 8-byte shader hashes (7346193), cooked import
   `PackageName` (195bf45), script import classes + `bIsAsset` (66f361d), PackageSource +
   SavedHash + script serialization offsets (0717d17), §6a.1/§6a.2.
   Measured: ~1 GB/s read + Oodle + hash (warm cache); 81 MB peak while reading a 3.5 GB chunk.
10. Writer (N1, 3a5f9d3 + e7e9a29) and container-header edits (86693a7), §6b.

## 12. Testing
- Test containers (project files `IOStore_UEVanilla_test_containers_v3.7z`,
  `IOStore_GameSpecific_test_containers_v1.7z`, `…_UE50_v2.7z`, kept outside the repo; folder in
  `IOSTORE_SAMPLES`, Oodle library folder in `IOSTORE_OODLE`, AES keys from `key.txt` next to a
  container). **[done]** Plugin tests (`cargo test --release`, through open → ReadHeader →
  ProcessFile): every sample lists every chunk exactly once, every item passes `PK_TEST`,
  extracted files have the listed size; `.utoc` info view; mount-point views and `$chunks`;
  global lookup rules (Paks boundary, limit, invalid global, `.ucas` without `.utoc`,
  partition names); key dialog flow (cancel, wrong key, right key, session cache); a damaged
  block fails `PK_TEST` but still extracts. Fork tests: see §11.
- **Trimmed containers** (`tools/ucas_trim.py`, Python 3.9+, stdlib only): big game containers
  are uploaded with the `.utoc` unchanged and the `.ucas` zeroed except for selected chunks;
  `<name>.trim.json` lists the chunks that still hold real data. Tests list the whole container
  and extract/hash-check only the kept chunks.
- **Oodle**: tests use the real library through the fork's `oodle_loader` search paths:
  `liboo2corelinux64.so.9` in the sandbox, `oo2core_9_win64.dll` on Windows (both project
  files, never committed; test path from an env var). The `.so` decoded every Oodle block of
  the 4.27, G1R and 5.0 samples with matching chunk hashes.
- Synthetic containers generated in the tests cover what the real sets don't (chunks > 4 GB,
  odd block sizes); real Oodle compression via the `.so`.
- Large containers, real data, fetched with anonymous Steam login (DepotDownloader, never
  uploaded):
  - **Default:** The Isle Dedicated Server, branch `evrima` (`tools/fetch_isle_server.sh`,
    ~1 GB): TOC v8, Oodle, encrypted + signed (same key as the TheIsle sample). pakchunk2 has
    36,575 chunks and a 2 MB directory index, pakchunk1 an 836 MB chunk. Every chunk of
    pakchunk2 passes its hash check (manifest 3189305585142138513).
  - **Only for chunks > 2 GB:** ARK: Survival Ascended Dedicated Server
    (`tools/fetch_ark_server.sh`, ~3.5 GB): pakchunk0 has 162,944 chunks, pakchunk10 a 3.5 GB
    chunk; all chunks pass (manifest 6268348989345964786).
- **Performance checks run only when needed** **[decided]**: when code that lists or reads
  containers changes in a way that could affect speed or memory, not on every change or
  release. Normal test runs use the small samples.
- Synthetic containers generated in the tests cover what the real sets don't (chunks > 4 GB,
  odd block sizes); real Oodle compression via the `.so`.
- Large containers, real data: ARK: Survival Ascended Dedicated Server (UE 5.x, TOC v8, Oodle),
  downloaded with anonymous Steam login by `tools/fetch_ark_server.sh` (DepotDownloader,
  file list `tools/ark_filelist.txt`, ~3.5 GB, never uploaded): `pakchunk0` has 162,944 chunks
  and an 8.3 MB directory index (listing speed), `pakchunk10` has a 3.5 GB chunk (streaming,
  memory, progress, Cancel). As of manifest 6268348989345964786 (2026-11-18) every chunk of
  pakchunk0/10/11 and global passes its hash check. Also: the trimmed 5.0 demo (9157 chunks).
- Compressed UE5 (`IOStore_UEVanilla58_test_containers_COMPRESSED.7z`): 5.8 with Zlib, Gzip
  and LZ4 pass. Its `Zstd` folder holds Zlib data (byte-identical to the `zlib` set): UE 5.8's
  packer seems to fall back to Zlib for Zstd, so no real Zstd sample exists yet (Zstd is covered
  by the synthetic tests only). Oodle on UE5 is covered by G1R, the 5.0 demo and TheIsle.
- Backburner: encrypted with a non-zero key GUID; optional segment (`.o.utoc`), partitions
  (`_sN.ucas`), `.m.ubulk`, localized packages (user will make these with a local UE install).
- Behaviour in TC (popups, Find Files, view modes) is checked by the user, as for uepak.
- **TC checks under Wine cover only what the current change touches** **[decided 2026-11-19]**;
  a full pass over all TC features runs only on request.

## 12a. Observed TC behaviour (TC 11.58.1 x64 under Wine 11, 2026-11-19)
Checked in TC itself (portable TC under WineHQ 11.0 + Xvfb in the sandbox; Ubuntu's Wine 9.0
crashes on start there):
- Association: `.ucas` and `.utoc` entries in `[PackerPlugins]` with caps 212; both show
  archive icons. Listing: mount folders and `$chunks` hidden, sizes > 4 GB shown correctly,
  `cd` into paths inside a container works.
- Extract (F5) and TC's Test archives: 5.8 files byte-identical to retoc's unpack; all 843 G1R
  (Oodle) files match their TOC hashes; Test archives on the encrypted 4.27 set: 31/31 hashes.
  A 3.5 GB chunk (ARK): progress + Cancel ("User abort!" from TC, partial file deleted), then a
  full Background extraction while browsing other containers; hash matches.
- `.utoc` info file in Lister (F3): all sections render.
- Key dialog: wrong key → error line, right key → listing, "session only" writes no keys file.
- Oodle DLL removed: one warning naming `deps\`; putting the DLL back works without restarting TC.
- **TC adds its own "Error in packed file" after any failed OpenArchive**, whatever the code
  (13, 14, 15, 21, 22, 24 tried); OpenResult 0 with no handle shows an empty archive instead. So
  the no-global warning (and a cancelled key prompt) is followed by TC's generic message.
  During Find Files TC adds no message. Keeping both messages **[decided 2026-11-19]**.
- Find Files over all samples (search inside archives): finds the files in every container. A
  cancelled key prompt now covers the folder + key GUID for 10 s (was: per container), so one
  prompt per folder: 3 popups instead of 5 for the sample set (no-global, 4.27, TheIsle).
- Under Wine the ⚠ in the key dialog's error line shows as a box (font); fine on Windows.
- Info file in Lister: fixed-width layout lines up (§5); Lister's wrap (key `W`) wraps at about
  75 characters, so the wide `[Chunks]` rows wrap until wrapping is switched off.
- `$legacy` (N3 check only, 2026-11-19): hidden folder shown next to `$chunks`; F5 of `$legacy`
  from the 5.8 sample (versioned): 22 files, identical to retoc, no prompt. The Isle server
  (unversioned 5.6): the engine-version dialog appears once (combo box with "UE 5.6" /
  "UE 5.7 / 5.8", newest preselected, *Remember* ticked); picking 5.6 wrote
  `LegacyEngineVersion=UE5_6` into `uetoc.ini` keeping the line's comment; 110 files identical to
  retoc; the next copy used the ini value without asking.

- **N1 packing (2026-11-19, 5.8 zlib sample copied to `C:\game`)**: F5 of 16 extracted files
  onto `pakchunk11` replaced all 16 and auto-compacted; chunk hashes identical to the original.
  Alt+F5 with an `.ini` among the files: one refusal message, nothing written. Alt+F5 of
  `BP_SML.uasset` to a new `zzz_mod_P.ucas`: TOC v8, block 64 KiB, mount
  `../../../Moadra/Content/Mods/SML/`, 347-byte `zzz_mod_P.pak`; opens in TC with `$chunks`,
  `$legacy` and the folder. F8 on `$chunks\…ContainerHeader`: refused with our message, then TC's
  "User abort!" (as uepak, `E_EABORTED`). F8 on `BP_SML.uasset`: "1 removed, rebuilt" (`.ucas`
  64 bytes).

- **Configure dialog (2026-11-20)**: opened from Alt+F5 → Configure with ucas selected. Shows the
  ini values; LZ4 greys out both level lists; unticking the `.pak` box enables the warning box;
  "ten" as size shows the error line and keeps the dialog open; OK with `20M` and
  `..\..\..\Moadra\Content\Mods` wrote `AutoCompactUnder=20M`,
  `NewMountPoint=../../../Moadra/Content/Mods/`, comments kept; reopening showed the saved values.
  A label was cut off under Wine's font at first (widened).

## 13. Open items
- Short check on real Windows (§12a was done under Wine): dialog layout/fonts, install from
  the CI package and `.utoc` association by hand (pluginst.inf names only `.ucas`).
- Block signature rule (TheIsle `Signed`).
- On-demand containers: how they look on disk, if any sample turns up.
