# N7: source stream on extracted packages (for tcl-uassetvw) **[implemented 2026-11-20]**

Proposal from the tcl-uassetvw project (Lister plugin for `.uasset/.umap` metadata,
<https://github.com/Civ01/tcl-uassetvw>), to be revised here before anything is implemented.
Marks as in `SPEC.md`: **[decided]** = user decision, **[proposed]** = open for revision.
The tcl-iostore review (findings checked under Wine, open questions answered, changes) is §8;
edits it caused in §2–§6 are marked *(rev)*.

## 1. Problem

When a package is viewed from inside a container (F3, Quick View, Enter), TC asks the WCX
plugin to extract that one file into its temp folder and opens Lister on the copy. Observed in
TC 11.58.1 under Wine (2026-11-20): F3 on `Moadra\Content\Mods\SML\BP_SML.uasset` inside
`SML.ucas` opens Lister on `c:\users\<user>\Temp\_tc\BP_SML.uasset`, a flat folder with no trace of
the container.

- **Raw Zen packages**: tcl-uassetvw names script imports and export classes from the game's
  `global.utoc` (and, next, other packages' exports from the container headers). It finds the
  Paks folder by looking next to the file, else the last used folder. From TC's temp folder it
  finds nothing, so unless the last used folder is the same game the view stays unresolved and
  asks the user to pick a folder — although tcl-iostore knew the container a moment earlier.
- **`$legacy` packages**: they display fully (the metadata is in the `.uasset`; TC extracts only
  that file, no `.uexp`). But an unversioned legacy header can fit several engine versions (5.7
  vs 5.8 share object version 1018 and differ only in the import layout), while tcl-iostore
  knows exactly which version it converted with (`LegacyEngineVersion` / the prompt's answer).

## 2. Proposal

On extraction, tcl-iostore attaches a small **named stream** to each extracted package file:
`<file>:uetoc.source`. tcl-uassetvw reads it when it opens a file and uses it as the first Paks
folder candidate (still subject to its fit checks) and, for `$legacy` files, as the engine version.

### 2.1 What a named stream is
An NTFS *alternate data stream*: extra named data attached to a file, next to its normal content
(Windows uses the same mechanism for the "downloaded from the internet" mark, `:Zone.Identifier`).

- Invisible to normal use: the file's size, content and hash are unchanged; programs that open the
  file read only the main content. `dir /r` (or tools that ask for streams) lists it.
- **NTFS and ReFS only.** FAT32/exFAT (USB sticks, SD cards) have no streams; network shares keep
  them when the server's volume is NTFS. Where streams aren't supported nothing is written and
  the viewer behaves as today.
- Copy/move NTFS→NTFS keeps the stream; zip/7z, git, FAT drop it. Explorer asks "copy without its
  properties?" when copying such a file to a FAT/exFAT drive — the reason for the default scope
  below.
- Deleted with the file.

### 2.2 When it is written **[proposed]**
- Only on `PK_EXTRACT` (never `PK_TEST`), after the main data is written, flushed and closed
  successfully.
- Only for package files: `.uasset` / `.umap` entries with an `ExportBundleData` chunk in the
  listing (raw Zen), and the converted `.uasset` / `.umap` in `$legacy`. Not for `.uexp`, bulk
  files, shader libraries, the info file, `$chunks` entries.
- Scope by `[Options] SourceStream=View|All|Off`, default **`View`**:
  - `View`: only when the destination is TC's view-extraction folder: the destination's parent
    folder name starts with `_tc` (case-insensitive) and its parent is the user's temp folder
    (`GetTempPathW`, compared after normalising). That is where F3 / Quick View / Enter put the
    file; TC deletes it afterwards, so no stream outlives the view and normal extractions (F5)
    never carry one (no Explorer warning, no local paths in files the user shares later).
  - `All`: every package extraction (files extracted earlier still open with names; note that the
    stream holds local absolute paths).
  - `Off`: never.
- Only when the destination volume reports named streams (`GetVolumeInformationW` on the
  destination's volume root → `FILE_NAMED_STREAMS`), cached per volume for the session.
  **Required, not optional** *(rev)*: verified under Wine 11 (§8.1), which reports no named
  streams for `C:` yet lets `CreateFileW("a.uasset:uetoc.source")` succeed and creates a stray,
  visible file of that name next to the package. On FAT/exFAT the same call fails cleanly.
- Only when the destination **file name** ends in `.uasset` / `.umap` *(rev)*: TC's text search
  in archives extracts entries to `%TEMP%\WCSxxxx.tmp` (observed, §8.1); with `All` those temp files
  would otherwise get a stream each.
- The main file's times are kept *(rev)*: read its creation/last-write times after closing it,
  write the stream, and set the same times back (`SetFileTime`), so the stream can't change the
  file's date on any file system or Windows version (neither the plugin nor TC set times on
  extracted files under Wine; on Windows TC may set them after `ProcessFile` returns, which is
  after the stream anyway).
- Failures (unsupported volume, access denied, …) are logged (`DebugLog`) and never change the
  extraction result.

### 2.3 Format **[proposed]**
UTF-8 without BOM, at most 4 KiB (longer content: no stream, logged *(rev)*), lines separated by LF (CRLF accepted), first line a format tag,
then `key=value` lines. Readers ignore unknown keys; a new incompatible format bumps the tag.

```
uetoc-source 1
form=raw
container=C:\Games\Moadra\Content\Paks\SML.utoc
paks=C:\Games\Moadra\Content\Paks
entry=../../../Moadra/Content/Mods/SML/BP_SML.uasset
chunk=5987b788f2a920550000000001
toc_hash=…
```

| Key | Required | Meaning |
|---|---|---|
| `form` | yes | `raw` (Zen package as stored) or `legacy` (converted, `$legacy`) |
| `container` | yes | absolute path of the opened container's `.utoc` |
| `paks` | yes | folder where the global container was found (§3.2 lookup result) |
| `entry` | yes | the entry's path in the listing, mount point included (`$legacy\…` for legacy) |
| `chunk` | raw + legacy | 24 hex digits: the package's `ExportBundleData` chunk id (the source package for legacy) |
| `toc_hash` | optional | the chunk hash from the TOC (hex), so a reader can tell whether the file was edited since |
| `engine` | legacy | engine version the conversion used, ini spelling (`UE5_8`, `UE4_27`, …): the ini value, the prompt's answer or the auto-detected version |
| `engine_range` | optional, raw | the engine versions tcl-iostore's `$legacy` view would offer for this container (`legacy::candidates` from the TOC and container-header versions), first and last in ini spelling (e.g. `UE5_6-UE5_8`); left out when that list is empty *(rev: §8.3, Q3)* |

Never written: AES keys or anything derived from them, user names beyond what the paths contain.

### 2.4 Packing (N1, N2) **[proposed]**
Unaffected. Packing reads files through their normal content only (the default stream, as
`std::fs::File::open` does), so streams never reach containers, chunk hashes or sizes, and a
file with a stream packs byte-identically to one without. N1 replaces by name and doesn't look at
streams; N2 (legacy input) likewise.

Possible later use (backburner, not part of N7): when packing a file whose stream names a
container of the target game, preselect that container in the pack dialog.

## 3. Consumer: tcl-uassetvw (for reference)

Specified in tcl-uassetvw's `SPEC.md` §6.4. In short:
- Reads `<file>:uetoc.source` (Windows; ≤ 4 KiB; unknown tag → ignored).
- Paks folder candidates: user-chosen → **`paks` from the stream** → `global.utoc` next to the file →
  last used. The stream's folder still has to pass the fit checks (header layout vs container
  versions, script imports present); when it fits it becomes the last used folder.
- `form=legacy`: `engine` decides the layout of an unversioned header (and 5.7 vs 5.8) when that
  layout fits; shown as "from the converter". A layout that doesn't fit is ignored with a note.
- `chunk`/`toc_hash`: input for the viewer's planned check 4 (own chunk in the containers).
- The stream's content is treated as untrusted data: paths are only used to read containers
  (read-only), as with the user's own last used folder.

## 4. Alternatives considered

- **In-process handoff**: both plugins run inside TC's process, so tcl-iostore could record
  "temp path → container" in memory (named shared memory or an environment variable) and the
  viewer would look its path up. Nothing on disk, file-system independent, but covers only views
  from inside the archive, needs a shared protocol for concurrent extractions, and doesn't survive
  a TC restart. Rejected in favour of the stream **[decided 2026-11-20, user]**.
- **Side-car file** (`BP_SML.uasset.source` next to the file): visible, TC doesn't delete it with
  the temp file, and it would be picked up by packing. Rejected.

## 5. Implementation sketch (tcl-iostore)

- `read.rs` `process()`: after a successful raw or legacy package write, call
  `source_stream::write(dest, &SourceInfo { … })` when `config.source_stream` allows it.
- `source_stream.rs`: scope check (`View`: `_tc*` under `GetTempPathW`), volume check
  (`GetVolumeInformationW` on the destination's root, `FILE_NAMED_STREAMS`), write
  `"<dest>:uetoc.source"` (via `CreateFileW`; check that std's path handling, `\\?\` prefix
  included, keeps the `:stream` suffix), format per §2.3.
- `config.rs`: `SourceStream=View|All|Off`; `uetoc.ini` comment line; README key table.
- The legacy path already knows the engine version used (`legacy.rs`); the raw path knows the
  container, the global's folder (`global.rs`) and the chunk id.

## 6. Tests **[proposed]**

- Unit tests (Windows CI, NTFS temp dir): stream written for a raw and a `$legacy` package with
  `All`; not for `PK_TEST`, `.uexp`, bulk, info file, `$chunks`; `View` writes only under a
  `_tc` folder in the temp folder; `Off` never; content round-trips; main file bytes and hash
  unchanged; packing a file with a stream gives the same chunk as without.
- Under Wine: no stray `…:uetoc.source` file appears next to extracted files (the volume check is
  what prevents it, §8.1) *(rev)*.
- Main file's last-write time unchanged by the stream (`All`, NTFS) *(rev)*.
- Text search inside a container (`WCS*.tmp` destinations): no stream, also with `All` *(rev)*.
- TC on real Windows (Wine has no streams): F3 on a raw package inside a container with
  tcl-uassetvw installed and no last used folder → names resolved without a prompt; F3 on a
  `$legacy` unversioned file → engine "from the converter"; F5 of the same files → no stream with
  `View`, stream with `All` (`dir /r`).
- TC on real Windows *(rev, §8.2)*: drag a package from inside a container to Explorer, and Ctrl+C
  in the container + paste in Explorer: note the extraction folder TC uses (log) and whether the
  pasted file carries the stream (`dir /r`).

## 7. Open questions

1. Stream name: `uetoc.source` (plugin's file name) or a neutral `ue.source` that other tools
   could adopt?
2. Should `View` also cover TC's other temp uses (e.g. "Open with" / Enter on an archive entry,
   which also extracts to `_tc`)? Proposed: yes, same folder.
3. `engine_range` for raw packages: worth adding, or leave version logic to the viewer?

## 8. Review (tcl-iostore, 2026-11-20)

Question from the user: can N7 be implemented without side effects? **Yes for everything
tcl-iostore produces** — the extracted file's bytes, size, hash and (with §2.2's time restore)
dates, packing (N1/N2), listings — with the changes marked *(rev)* above. One user-visible effect
remains open until checked on Windows: §8.2.

### 8.1 Checked under Wine 11 / TC 11.58.1 (sandbox)
- **F3 / Quick View** inside a container: `ProcessFile(PK_EXTRACT)` with
  `C:\users\<user>\AppData\Local\Temp\_tc\BP_SML.uasset` (flat), as §1 says.
- **Find Files with text** inside a container: every entry extracted to
  `C:\users\<user>\AppData\Local\Temp\WCSxxxx.tmp` (one reused name) — not `_tc`, so `View` never
  writes there; the file-name rule (§2.2) keeps `All` out as well.
- **Ctrl+C** on a file inside a container: nothing is extracted at copy time (TC defers until a
  paste; no Explorer under Wine to paste into).
- **Named streams under Wine** (small test program, `C:` on the Wine prefix): `GetVolumeInformationW`
  → file system "NTFS", flags `0x8a`, **no** `FILE_NAMED_STREAMS`; `std::fs::write("…\a.uasset:uetoc.source")`
  succeeds, reads back, and leaves a second, visible file `a.uasset:uetoc.source` in the folder;
  the main file's size, content and last-write time are unchanged.
- **Times**: extracted files carry their extraction time (sub-second values differ per file):
  neither the plugin nor TC sets file times on extraction there.
- **Packing** (from the code, not a Wine run): `PackFiles` — F5 into a container, or TC's
  "update archive" after editing an extracted file — opens sources with `std::fs::File::open`
  (default stream only), as does N1's `ValidateOnReplace`. A stream can't reach a container.

### 8.2 Open: copy out of an archive (needs real Windows)
If TC extracts to its `_tc` folder for **drag & drop to Explorer** or **paste from clipboard**,
the file the user drops somewhere would carry the stream (local paths of the game) and Explorer
would show "copy without properties?" when the target is FAT/exFAT. Not checkable under Wine
(§8.1). If confirmed, options: (a) accept — the stream is harmless and small; (b) default
`SourceStream=Off`, uassetvw users switch it on; (c) find a narrower signal for views if TC uses a
different folder or name pattern for drags. Decide after the Windows check; until then `View`
stays the proposed default.

### 8.3 Open questions answered
1. **Stream name**: keep `uetoc.source`. It names the writer, the format tag inside carries the
   version, and another tool can read it by that name; a neutral `ue.source` would suggest a
   shared format nobody else has adopted.
2. **`View` scope**: yes, anything TC extracts into `_tc*` under the temp folder (F3, Quick View,
   Enter / Open with). Text search (`WCS*.tmp`) is outside it by observation.
3. **`engine_range`**: add it. tcl-iostore already computes the candidate list for the `$legacy`
   prompt (`legacy::candidates`), so it costs nothing and gives the viewer the same range the
   converter would offer, instead of a second copy of that logic. Leave it out when the list is
   empty.

### 8.4 Other notes
- `View` detection fails safe: if TC's temp folder isn't `GetTempPathW` (TEMP changed for TC),
  nothing matches and no stream is written.
- Performance: one extra small write per viewed package; the volume check is cached per volume.
- Some security products log alternate-data-stream writes; the stream holds only paths and ids.
- Side note from the PR description (Ubuntu's Wine 9.0 needs `/run/user/<uid>`): taken over into
  `SPEC.md` §12a when N7 lands; the sandbox here runs WineHQ 11.

## 9. As built (tcl-iostore, 2026-11-20)
- `src/source_stream.rs` (scope, volume check cached per volume, write + time restore, content),
  called from `read.rs` after a successful raw package extraction (`ExportBundleData` chunk)
  and after a `$legacy` `.uasset`/`.umap`. `[Options] SourceStream=View|All|Off`, default `View`.
- `View` = the destination is anywhere under `<temp>\_tc*\` (`std::env::temp_dir()`, i.e.
  `GetTempPath2W`/`GetTempPathW`; `\\?\` prefix and case ignored).
- `engine` (legacy) is written when the conversion used a fixed engine version (ini, prompt or
  the container's versions); a versioned package converted with its own versions has none —
  its header carries the versions itself. `engine_range`: legacy = the `$legacy` prompt's list
  (`Session::engine_candidates`); raw = `legacy::candidates` for the container's TOC and
  container-header versions (computed once per opened container, only when a stream is wanted).
- Main file times: read before the stream is written, set back after (`File::set_times` with
  created/modified/accessed).
- Tests: scope and content unit tests; `source_stream_fields_and_scope` (samples: raw + legacy
  fields, View only under `_tc*`, Off never, extracted bytes identical with and without);
  Windows CI: real stream round trip with unchanged bytes and times. TC under Wine: F3 → scope
  matched, no stream, no stray file.
- Still open: §8.2 (drag & drop / paste out of an archive on real Windows).

