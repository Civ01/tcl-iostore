# tcl-iostore — Specification

Total Commander 11+ packer plugin (`uetoc.wcx64`) for Unreal Engine IoStore containers
(`.utoc` + `.ucas`). Sister of [tcl-uepak](https://github.com/Civ01/tcl-uepak) (`uepak.wcx64`,
legacy `.pak`), whose skeleton, conventions and lessons it reuses.

Spec version: v0 (2026-11-18), the reading/extraction phase. Marks: **[decided]** = user decision,
**[proposed]** = my default, change it if you disagree, **[open]** = needs input or samples.

---

## 0. Status and roadmap

Nothing implemented yet.

### Phase 1: reading and extraction (current)
| # | Milestone | Where |
|---|---|---|
| R1 | Container reader in the fork: partitions, block-by-block streaming, AES, clear errors, trimmed dependencies, Oodle from Civ01/repak | `Civ01/retoc` branch `tcl-iostore` |
| R2 | Plugin skeleton (from tcl-uepak), open `.ucas`/`.utoc`, global lookup, listing, `$chunks` folder, `.utoc` info file | plugin |
| R3 | Extract and test (hash checks), AES keys (`uetoc.keys.json`, key dialog), Oodle `deps\` | plugin |
| R4 | Game-profile hook (vanilla profile only; see §2.2) wired through every read path | both |

### Later goals
- Packing: new containers, add/replace/delete, raw Zen input and legacy `.uasset/.uexp` input
  (auto-detected, else ask once per copy session) **[decided]**, companion `.pak` written by repak.
- Custom game support based on CUE4Parse's game-specific code (keys, custom encryption, block
  layouts, header magics), picked via a game profile, probably prompted on a read failure **[decided]**.
- TOC v9 `AddedSourceHashes` / v10 `ContainerEncryptionMethod` (AES-CTR) once a released engine
  writes them (in CUE4Parse; not in UE 5.8.4).
- Optional legacy conversion on extract (retoc `to-legacy`).

### Non-goals (phase 1)
- Writing anything. Packing operations return `E_NOT_SUPPORTED` and the capabilities don't
  advertise them.
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
| 3 | PartitionSize | 4.27 | vanilla 4.27 (Oodle), encrypted 4.27 (Oodle; key needed) |
| 4 | PerfectHash | (early 5.0 previews) | none |
| 5 | PerfectHashWithOverflow | 5.0–5.3 | VanySlash demo 5.0 (trimmed, 9157 chunks, Oodle); vanilla 5.1, 5.2, 5.3 (uncompressed) |
| 6 | OnDemandMetaData | 5.4 | vanilla 5.4, 5.4 AngelScript fork, G1R (Oodle) |
| 7 | RemovedOnDemandMetaData | (unreleased) | none |
| 8 | ReplaceIoChunkHashWithIoHash | 5.5–5.8 | vanilla 5.5–5.8; TheIsle 5.6 (encrypted, signed, Oodle) |
| 9, 10 | AddedSourceHashes, ContainerEncryptionMethod | after 5.8.4 | none; refused with "TOC version N is newer than this plugin supports" |

- Header 0x90 bytes, magic `-==--==--==--==-`. A header-size or block-entry-size mismatch is an
  error, not an assert.
- Container header versions: −1 PreInitial … 5 SoftPackageReferencesOffset (5.6+).
- **Chunk hashes** (`FIoStoreTocEntryMeta`), measured on the test containers: **SHA-1** of the
  uncompressed chunk in 4.27; **BLAKE3 truncated to 20 bytes** in 5.0–5.8 (v5, v6, v8).
- Blocks: each block's disk size is padded to 16 bytes with `buf[(i - size) % size]` (same rule
  as pak); AES-256-ECB over the padded block when encrypted; the directory index is encrypted
  with the same key. An offset in the uncompressed address space / block size = block index; a
  block offset / `PartitionSize` = partition index (`name.ucas`, `name_s1.ucas`, …).
- A container is mounted through the `.pak` of the same name; the optional segment is `.o.utoc`.

---

## 2. Architecture

### 2.1 Layers
```
uetoc.wcx64 (cdylib, this repo)
 ├─ ffi.rs       TC exports (W + ANSI), panic guard on each export (from uepak)
 ├─ open.rs      .ucas/.utoc resolution, global lookup, archive handle
 ├─ listing.rs   view model: mount-point view modes, $chunks folder, info file
 ├─ info.rs      info-file text renderer
 ├─ extract.rs   streaming extract/test, progress, cancel
 ├─ keys.rs      uetoc.keys.json + key dialog (from uepak)
 ├─ profile.rs   game profiles (§2.2)
 └─ settings/log/dialog/win/busy  (from uepak)
retoc (Civ01/retoc, branch tcl-iostore)
 └─ container layer: TOC parse, partitions, block reader, directory index,
    container header, Zen parsing (later: writer, conversion)
```
Dependency: `retoc = { git = "https://github.com/Civ01/retoc", branch = "tcl-iostore",
default-features = false }`. Build: `x86_64-pc-windows-gnu`, cdylib only (LTO; see the uepak
gotcha), stripped.

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
(`ScriptObjects` in 5.x, `LoaderGlobalMeta` + `LoaderGlobalNames` in 4.27). An encrypted global
uses the normal key lookup.

**Version mismatch** (global TOC version ≠ container TOC version): accepted with a warning
**[decided]**. The warning is logged every time and shown as a popup once per
(global, container folder) per TC session **[proposed]**, so Find Files over a folder doesn't
show one per container.

**Not found:** warning naming the folders searched and why the search stopped (limit or `Paks`
boundary), then the archive isn't opened (`E_UNKNOWN_FORMAT`) **[decided]**. Cached per folder
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

## 5. Info file (`<ContainerId>.txt`) **[decided]**, layout **[proposed]**
Generated in memory on open; UTF-8 with BOM, CRLF; listed with its real size and the `.utoc`
date; extract/F3 gives the text. Sections:

```
[Container]   file names (utoc, ucas + partitions), sizes, companion .pak present yes/no,
              global used (path, TOC version, match/mismatch)
[Header]      every header field: magic, version (number + name), header size, entry count,
              block count + entry size, compression method names, block size, directory
              index size, partition count/size, container id (dec + hex), key GUID, flags,
              perfect-hash seed count, chunks without perfect hash
[ContainerHeader]  version, package count, localized packages, redirects (if parsed)
[Signatures]  present yes/no, hash size (not verified: needs the game's public key)
[Files]       one line per chunk, directory-index order then unnamed chunks:
              path | chunk id | type | offset | size | packed | blocks | compression | flags (Compressed, MemoryMapped) | hash
```
Columns tab-separated so the text pastes into a spreadsheet. If the directory index is
encrypted and no key is available, the `[Files]` paths read `(encrypted)` and the file still
opens **[proposed]**.

## 6. Extraction and testing

As uepak §3.3:
- Streaming one block at a time (decrypt the 16-aligned block, decompress), constant memory,
  progress and Cancel per block; partial files deleted on failure.
- Errors: create → `E_ECREATE`, write → `E_EWRITE`, decode/hash → `E_BAD_DATA`, missing Oodle →
  warning + `E_NOT_SUPPORTED`, key prompt cancelled → `E_EABORTED`.
- `PK_TEST`: decode every block and check the chunk hash (SHA-1 for TOC v3, BLAKE3-160 for v5+;
  a chunk whose hash is all zero is only decoded). Block signatures (`Signed`) are reported in the
  info file but not checked **[proposed]**: retoc notes they don't match a plain SHA-1 of the
  stored bytes; to be investigated with TheIsle.

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
VirtualFolderAttr=Hidden    ; attribute of $chunks
DebugLog=0
```
No settings dialog in phase 1 **[proposed]**; `ConfigurePacker` opens a message box naming the
ini file.

## 10. Diagnostics and robustness
Taken from uepak: panic guard on every export, `DebugLog=1` → `uetoc.log` or in-memory log +
clipboard on error, background unpack (`GetBackgroundFlags = UNPACK`) with per-handle state,
one key dialog at a time, popups without an owner in background threads.

## 11. Fork changes (`Civ01/retoc`, branch `tcl-iostore`; `master` untouched) **[decided]**
1. Reader: open every partition; block-by-block read API (`ChunkReader: Read`) with progress
   and cancel hooks; header/entry-size checks instead of asserts; typed errors
   (`thiserror`) instead of `anyhow` at the API boundary.
2. Dependencies: `clap`, `rayon`, `indicatif`, `jmap`, `tracing`, conversion code behind
   features (`cli`, `convert`), so the plugin build pulls in only the container layer.
3. Compression: `oodle_loader`/`repak` from Civ01/repak (`oodle-search-paths`), no download.
4. Hash check helpers (SHA-1 / BLAKE3-160 by version), `FIoContainerId` and chunk-type names.
5. `GameProfile` trait with a `Vanilla` implementation (§2.2).
6. No panics on unexpected input (e.g. retoc panics on an unknown chunk type today).
7. Tests against the project's test containers (kept outside the repo; path from an env var)
   plus small synthetic containers in the repo.

## 12. Testing
- Test containers (project files `IOStore_UEVanilla_test_containers_v2.7z`,
  `IOStore_GameSpecific_test_containers_v1.7z`, kept outside the repo): every container lists,
  every chunk extracts and passes its hash check, the info file renders; encrypted TheIsle with
  its key, and a wrong key is rejected.
- **Trimmed containers** (`tools/ucas_trim.py`, Python 3.9+, stdlib only): big game containers
  are uploaded with the `.utoc` unchanged and the `.ucas` zeroed except for selected chunks;
  `<name>.trim.json` lists the chunks that still hold real data. Tests list the whole container
  and extract/hash-check only the kept chunks.
- **Oodle**: tests use the real library through the fork's `oodle_loader` search paths:
  `liboo2corelinux64.so.9` in the sandbox, `oo2core_9_win64.dll` on Windows (both project
  files, never committed; test path from an env var). The `.so` decoded every Oodle block of
  the 4.27, G1R and 5.0 samples with matching chunk hashes.
- Streaming of very large chunks (hundreds of MB) is tested with synthetic containers generated
  in the tests, since such chunks can't be uploaded.
- Large container: the trimmed 5.0 demo (9157 chunks, 5685 packages, 333 KB directory index,
  1.3 GB `.ucas`) covers listing speed.
- Missing samples **[open]**: one UE5 version (5.6–5.8) packed with Oodle, Zlib and LZ4
  (user uploads as we go); the key for the encrypted 4.27 sample.
- Backburner: encrypted with a non-zero key GUID; optional segment (`.o.utoc`), partitions
  (`_sN.ucas`), `.m.ubulk`, localized packages (user will make these with a local UE install).
- Behaviour in TC (popups, Find Files, view modes) is checked by the user, as for uepak.

## 13. Open items
- Block signature rule (TheIsle `Signed`).
- On-demand containers: how they look on disk, if any sample turns up.
