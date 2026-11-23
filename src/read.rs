//! Opening (file resolution, global lookup, AES keys), listing, extract and test.

use std::collections::HashSet;
use std::ffi::c_int;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use iostore::{Container, Error as IoError, HashCheck};

use crate::archive::{Archive, ItemKind};
use crate::ffi::*;
use crate::{config, dialog, dlog, global, keys, log, progress, show_error, show_warning, win};

// ---------------------------------------------------------------------------
// Which .utoc, and how to show it

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// Full listing (`.ucas`, or `.utoc` with `OpenUtocAsVirtualFile=0`).
    Listing,
    /// `<ContainerId>.txt` only (`.utoc`).
    Info,
}

fn ext_is(p: &Path, e: &str) -> bool {
    p.extension().is_some_and(|x| x.to_string_lossy().eq_ignore_ascii_case(e))
}

/// The `.utoc` for an opened `.ucas`: same name, or for a partition `X_s1.ucas` the `X.utoc`.
pub fn utoc_for_ucas(ucas: &Path) -> Option<PathBuf> {
    let same = ucas.with_extension("utoc");
    if same.is_file() {
        return Some(same);
    }
    let stem = ucas.file_stem()?.to_string_lossy().into_owned();
    let (base, n) = stem.rsplit_once("_s")?;
    if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) {
        let p = ucas.with_file_name(format!("{base}.utoc"));
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn has_toc_magic(p: &Path) -> bool {
    use std::io::Read;
    let mut m = [0u8; 16];
    std::fs::File::open(p).and_then(|mut f| f.read_exact(&mut m)).is_ok() && &m == iostore::toc::TOC_MAGIC
}

/// CanYouHandleThisFile: a `.utoc` with IoStore magic, or a `.ucas` whose `.utoc` has it.
pub fn looks_like_container(p: &Path) -> bool {
    if ext_is(p, "ucas") {
        return utoc_for_ucas(p).is_some_and(|u| has_toc_magic(&u));
    }
    has_toc_magic(p)
}

// ---------------------------------------------------------------------------
// One-time warnings

static WARNED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// True the first time `key` is seen this TC session.
fn first_time(key: String) -> bool {
    WARNED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashSet::new).insert(key)
}

/// Folders whose "no global" warning was shown recently (TC opens an archive several times).
static NO_GLOBAL_SHOWN: Mutex<Vec<(PathBuf, Instant)>> = Mutex::new(Vec::new());
const REPEAT_GRACE: Duration = Duration::from_secs(10);

fn recently_shown(list: &Mutex<Vec<(PathBuf, Instant)>>, p: &Path) -> bool {
    let mut l = list.lock().unwrap_or_else(|e| e.into_inner());
    l.retain(|(_, t)| t.elapsed() < REPEAT_GRACE);
    if l.iter().any(|(x, _)| x == p) {
        return true;
    }
    l.push((p.to_path_buf(), Instant::now()));
    false
}

fn oodle_missing_text(container: &Path, detail: &str) -> String {
    let deps = config::deps_dir().map(|d| d.display().to_string()).unwrap_or_else(|| "<plugin folder>\\deps".into());
    format!(
        "{}\n\nThis container uses Oodle compression, but the Oodle library was not found.\n\
         Copy oo2core_N_win64.dll (usually next to the game .exe or in \
         Engine\\Binaries\\ThirdParty\\Oodle) into:\n{deps}\n\n\
         Listing works; extracting Oodle-compressed files will fail until then.\n\n({detail})",
        container.display()
    )
}

pub(crate) fn warn_oodle_once(container: &Path, detail: &str) {
    if first_time(format!("oodle|{}", container.display())) {
        show_warning(&oodle_missing_text(container, detail));
    }
}

// ---------------------------------------------------------------------------
// AES keys (same lookup and dialog as tcl-uepak, SPEC §10)

static KEY_PROMPT: Mutex<()> = Mutex::new(());
/// Cancelled key prompts by (folder, key GUID): TC opens a container several times per
/// operation, and Find Files opens every container of a folder; one cancel covers them all
/// for a few seconds.
pub(crate) static DECLINED: Mutex<Vec<(PathBuf, String, Instant)>> = Mutex::new(Vec::new());

fn recently_declined(folder: &Path, guid: &str) -> bool {
    let mut d = DECLINED.lock().unwrap_or_else(|e| e.into_inner());
    d.retain(|(_, _, t)| t.elapsed() < REPEAT_GRACE);
    d.iter().any(|(f, g, _)| f == folder && g == guid)
}

fn mark_declined(folder: &Path, guid: &str) {
    DECLINED.lock().unwrap_or_else(|e| e.into_inner()).push((folder.to_path_buf(), guid.to_string(), Instant::now()));
}

fn save_key(folder: &Path, guid: &str, key: &keys::AesKey, persist: bool) {
    match keys::remember(folder, guid, key, persist) {
        Ok(()) => dlog!("  key remembered for {} ({})", folder.display(), if persist { "saved" } else { "this session" }),
        Err(e) => {
            dlog!("  could not save key: {e}");
            show_warning(&format!(
                "The key works but could not be saved to {}:\n{e}\n\nIt is kept until Total Commander is closed.",
                keys::KEYS_NAME
            ));
        }
    }
}

fn stale_key_message(guid: &str, from: &str) -> String {
    if guid != keys::ZERO_GUID {
        format!(
            "The key saved for this key GUID doesn't match this container. The GUID is the same, so \
             the container was most likely re-encrypted with a new key by a game update. Enter the \
             new key. (Saved for: {from})"
        )
    } else {
        format!(
            "The key saved for this folder doesn't match this container; the game may have been \
             updated with a new key. Enter the new key. (Saved for: {from})"
        )
    }
}

#[derive(Debug)]
pub enum KeyOutcome {
    Cancelled,
    Failed(IoError),
}

/// Finds a key for `c`: remembered keys first, then the dialog until a key fits or the
/// user cancels. With an encrypted directory index every key is checked; otherwise the
/// first remembered key is taken (checked by decoding data later).
pub fn resolve_key(c: &mut Container, opened: &Path) -> Result<(), KeyOutcome> {
    if c.has_key() {
        return Ok(());
    }
    let _prompt = KEY_PROMPT.lock().unwrap_or_else(|e| e.into_inner());
    let utoc = c.utoc_path().to_path_buf();
    let folder = utoc.parent().unwrap_or(Path::new("")).to_path_buf();
    let guid = c.header().key_guid.to_string();
    dlog!("  encrypted container, key guid {guid}; looking for a key");
    let mut stale: Option<String> = None;
    for (key, source, from) in keys::candidates(&folder, &guid) {
        match c.set_key(key.to_iostore()) {
            Ok(()) => {
                dlog!("  key from {source:?} record ({from}) {}", if c.index_needs_key() { "matches" } else { "used (can't be checked until data is read)" });
                if source == keys::Source::Guid {
                    save_key(&folder, &guid, &key, false);
                }
                return Ok(());
            }
            Err(IoError::WrongKey) => {
                dlog!("  key from {source:?} record ({from}) does not match");
                stale.get_or_insert(from);
            }
            Err(e) => return Err(KeyOutcome::Failed(e)),
        }
    }
    if recently_declined(&folder, &guid) {
        dlog!("  key prompt was cancelled moments ago; not asking again");
        return Err(KeyOutcome::Cancelled);
    }
    let mut error = stale.map(|from| stale_key_message(&guid, &from));
    loop {
        let Some(answer) = dialog::prompt_key(&dialog::KeyPrompt { container: opened, guid: &guid, error: error.as_deref() }) else {
            dlog!("  key prompt cancelled");
            mark_declined(&folder, &guid);
            return Err(KeyOutcome::Cancelled);
        };
        let key = match keys::AesKey::parse(&answer.text) {
            Ok(k) => k,
            Err(msg) => {
                error = Some(msg.to_string());
                continue;
            }
        };
        match c.set_key(key.to_iostore()) {
            Ok(()) => {
                dlog!("  entered key accepted");
                save_key(&folder, &guid, &key, answer.remember);
                return Ok(());
            }
            Err(IoError::WrongKey) => {
                dlog!("  entered key does not match");
                error = Some("This key does not match this container.".into());
            }
            Err(e) => return Err(KeyOutcome::Failed(e)),
        }
    }
}

// ---------------------------------------------------------------------------
// Open

fn open_error_code(path: &Path, e: &IoError) -> c_int {
    match e {
        IoError::Io(io) if matches!(io.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied) => E_EOPEN,
        IoError::Io(_) => E_EREAD,
        IoError::UnknownMagic => E_UNKNOWN_FORMAT,
        IoError::UnsupportedVersion(v) => {
            show_warning(&format!("{}\n\nTOC version {v} is newer than this plugin supports.", path.display()));
            E_UNKNOWN_FORMAT
        }
        _ => E_BAD_ARCHIVE,
    }
}

/// Opens `path` (`.ucas` or `.utoc`) as TC asked. `Err` carries the TC error code.
pub fn open_archive(path: &Path, options: &config::Options, list_mode: bool) -> Result<Archive, c_int> {
    let (utoc, view) = if ext_is(path, "ucas") {
        match utoc_for_ucas(path) {
            Some(u) => (u, View::Listing),
            None => {
                dlog!("  no .utoc next to the .ucas");
                show_warning(&format!(
                    "{}\n\n{} was not found next to it. The .ucas file holds only data; its .utoc is needed to read it.",
                    path.display(),
                    path.with_extension("utoc").file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
                ));
                return Err(E_EABORTED);
            }
        }
    } else {
        (path.to_path_buf(), if options.open_utoc_as_virtual_file { View::Info } else { View::Listing })
    };
    dlog!("  utoc {} view={view:?}", utoc.display());
    crate::legacy::note_opened(&utoc);
    if !list_mode || view == View::Listing {
        crate::pack::check_pending_undo(&utoc);
    }

    let started = Instant::now();
    let mut c = Container::open(&utoc, crate::profile::profile_for(&utoc)).map_err(|e| {
        dlog!("  open failed: {e}");
        open_error_code(path, &e)
    })?;
    let h = c.header().clone();
    dlog!(
        "  TOC v{} ({}) entries={} blocks={} block_size={} flags=[{}] methods={:?} partitions={} guid={}",
        h.version.0,
        h.version.name(),
        h.entry_count,
        h.block_count,
        h.block_size,
        h.flags,
        c.toc().compression_methods,
        h.effective_partition_count(),
        h.key_guid
    );

    // global container (SPEC §3.2)
    let folder = utoc.parent().unwrap_or(Path::new("")).to_path_buf();
    let global = match global::find_global(&utoc, options.global_lookup_limit) {
        Ok(g) => g,
        Err(nf) => {
            dlog!("  no valid global: searched {:?}, stop {:?}, rejected {:?}", nf.searched, nf.stop, nf.rejected);
            if !recently_shown(&NO_GLOBAL_SHOWN, &folder) {
                show_warning(&global::not_found_text(path, &nf));
            }
            return Err(E_EABORTED);
        }
    };
    dlog!("  global: {} (TOC v{})", global.utoc.display(), global.version.0);
    if global.version != h.version {
        dlog!("  global TOC version {} differs from the container's {}", global.version.0, h.version.0);
        if first_time(format!("globalver|{}|{}", global.utoc.display(), folder.display())) {
            show_warning(&format!(
                "{}\n\nThe global container {} has TOC version {}, this container has version {}. \
                 They usually come from the same engine build; the container is opened anyway.",
                path.display(),
                global.utoc.display(),
                global.version.0,
                h.version.0
            ));
        }
    }

    // AES key for the directory index
    let mut index_error: Option<String> = None;
    if c.index_needs_key() {
        match resolve_key(&mut c, path) {
            Ok(()) => {}
            Err(KeyOutcome::Cancelled) if view == View::Info => index_error = Some("encrypted (no key given)".into()),
            Err(KeyOutcome::Cancelled) => return Err(E_EABORTED),
            Err(KeyOutcome::Failed(e)) => {
                dlog!("  key check failed: {e}");
                return Err(E_BAD_ARCHIVE);
            }
        }
    }

    if list_mode {
        if c.compression_methods().contains(&iostore::Method::Oodle) {
            match iostore::oodle_status() {
                Ok(p) => dlog!("  oodle: loaded {}", p.display()),
                Err(e) => {
                    dlog!("  oodle: NOT available: {e}");
                    warn_oodle_once(path, &e);
                }
            }
        }
        let missing = c.missing_partitions();
        if !missing.is_empty() && first_time(format!("parts|{}", utoc.display())) {
            let list: Vec<String> = missing.iter().map(|p| p.display().to_string()).collect();
            dlog!("  missing data files: {list:?}");
            show_warning(&format!(
                "{}\n\nData file(s) of this container are missing:\n{}\n\nListing works; extracting files stored there will fail.",
                path.display(),
                list.join("\n")
            ));
        }
        if h.flags.on_demand() {
            dlog!("  OnDemand container: data reads will be refused");
        }
    }

    let arc = match view {
        View::Info => {
            if index_error.is_none() {
                if let Err(e) = c.directory_index() {
                    dlog!("  directory index: {e}");
                    index_error = Some(e.to_string());
                }
            }
            let text = crate::info::render(&c, Some(&global), index_error.as_deref());
            Archive::info_view(path, c, options, text)
        }
        View::Listing => {
            let (arc, skipped) = Archive::listing(path, c, options).map_err(|e| {
                dlog!("  directory index failed: {e}");
                if crate::profile::could_be_custom_game(&e) {
                    dlog!("  (a custom game profile might read it: not available yet)");
                }
                show_error(&format!("{}\n\nThe file list can't be read: {e}", path.display()));
                E_EABORTED
            })?;
            if skipped > 0 {
                dlog!("  {skipped} directory entries skipped (unsafe paths or bad entry numbers)");
            }
            arc
        }
    };
    let mut arc = arc;
    arc.global = Some(global.utoc.clone());
    dlog!("  opened in {} ms: {} items ({} files)", started.elapsed().as_millis(), arc.items.len(), arc.file_count());
    Ok(arc)
}

// ---------------------------------------------------------------------------
// ReadHeader

pub struct HeaderOut {
    pub name: String,
    pub pack: u64,
    pub unpack: u64,
    pub time: c_int,
    pub attr: c_int,
}

pub fn next_header(arc: &mut Archive) -> Option<HeaderOut> {
    let time = arc.mtime.map(win::dos_time).unwrap_or(0);
    let (mount_attr, virt_attr) = (arc.mount_folder_attr as c_int, arc.virtual_folder_attr as c_int);
    let item = arc.next_item()?;
    let attr = match item.kind {
        ItemKind::MountDir => FILE_ATTR_DIRECTORY | mount_attr,
        ItemKind::ChunksDir | ItemKind::LegacyDir => FILE_ATTR_DIRECTORY | virt_attr,
        _ => FILE_ATTR_ARCHIVE,
    };
    Some(HeaderOut { name: item.display.clone(), pack: item.pack, unpack: item.unpack, time, attr })
}

// ---------------------------------------------------------------------------
// ProcessFile

enum Fail {
    Container(IoError),
    Create(std::io::Error),
    Write(std::io::Error),
    Aborted,
    Hash,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Streams chunk `index` into `out` (None = test), reporting progress per block.
fn stream_chunk(arc: &Archive, index: u32, mut out: Option<&mut std::fs::File>, name: &mut [u16]) -> Result<HashCheck, Fail> {
    let mut r = arc.container.open_chunk(index).map_err(Fail::Container)?;
    while let Some(piece) = r.next_piece().map_err(Fail::Container)? {
        if let Some(f) = out.as_deref_mut() {
            f.write_all(piece).map_err(Fail::Write)?;
        }
        if !progress::report(name, piece.len() as u64) {
            return Err(Fail::Aborted);
        }
    }
    Ok(r.hash_check())
}

pub fn process(arc: &mut Archive, op: c_int, dest: Option<PathBuf>) -> c_int {
    if op == PK_SKIP {
        return 0;
    }
    let Some(item) = arc.current_item().cloned() else { return E_BAD_ARCHIVE };
    dlog!("ProcessFile {} {} dest={dest:?}", crate::op_name(op), item.display);
    if op != PK_EXTRACT && op != PK_TEST {
        return E_NOT_SUPPORTED;
    }
    let started = Instant::now();
    match item.kind {
        ItemKind::Legacy(kind) => process_legacy(arc, op, dest, &item.display, kind),
        ItemKind::MountDir | ItemKind::ChunksDir | ItemKind::LegacyDir => {
            if op == PK_EXTRACT {
                if let Some(d) = &dest {
                    if let Err(e) = std::fs::create_dir_all(d) {
                        dlog!("  create folder failed: {e}");
                        return E_ECREATE;
                    }
                }
            }
            0
        }
        ItemKind::InfoFile => {
            if op == PK_TEST {
                return 0;
            }
            let Some(d) = dest else { return E_ECREATE };
            let bytes = arc.info.as_deref().unwrap_or_default();
            if let Some(parent) = d.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::fs::write(&d, bytes) {
                Ok(()) => {
                    let mut name = win::wide(&d.to_string_lossy());
                    progress::report(&mut name, bytes.len() as u64);
                    0
                }
                Err(e) => {
                    dlog!("  write failed: {e}");
                    let _ = std::fs::remove_file(&d);
                    E_EWRITE
                }
            }
        }
        ItemKind::Chunk(index) => {
            if arc.container.is_encrypted() && !arc.container.has_key() {
                if arc.key_declined {
                    return E_EABORTED;
                }
                let opened = arc.path.clone();
                match resolve_key(&mut arc.container, &opened) {
                    Ok(()) => {}
                    Err(KeyOutcome::Cancelled) => {
                        arc.key_declined = true;
                        return E_EABORTED;
                    }
                    Err(KeyOutcome::Failed(e)) => {
                        dlog!("  key: {e}");
                        return E_BAD_DATA;
                    }
                }
            }
            let result = if op == PK_TEST {
                let mut name = win::wide(&item.display);
                stream_chunk(arc, index, None, &mut name)
            } else {
                let Some(d) = dest.clone() else { return E_ECREATE };
                if let Some(parent) = d.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                match std::fs::File::create(&d) {
                    Err(e) => Err(Fail::Create(e)),
                    Ok(mut f) => {
                        let mut name = win::wide(&d.to_string_lossy());
                        let r = stream_chunk(arc, index, Some(&mut f), &mut name);
                        let r = r.and_then(|h| f.flush().map(|_| h).map_err(Fail::Write));
                        drop(f);
                        if r.is_err() {
                            let _ = std::fs::remove_file(&d);
                        }
                        r
                    }
                }
            };
            let result = match result {
                Ok(HashCheck::Mismatch { expected, actual }) => {
                    dlog!("  hash mismatch: TOC {} data {}", hex(&expected), hex(&actual));
                    if op == PK_TEST {
                        Err(Fail::Hash)
                    } else {
                        // extraction keeps the data (a custom game may hash differently)
                        Ok(())
                    }
                }
                Ok(h) => {
                    dlog!("  hash: {h:?}");
                    Ok(())
                }
                Err(e) => Err(e),
            };
            let code = match result {
                Ok(()) => 0,
                Err(Fail::Aborted) => E_EABORTED,
                Err(Fail::Create(e)) => {
                    dlog!("  create failed: {e}");
                    E_ECREATE
                }
                Err(Fail::Write(e)) => {
                    dlog!("  write failed: {e}");
                    E_EWRITE
                }
                Err(Fail::Hash) => E_BAD_DATA,
                Err(Fail::Container(e)) => {
                    dlog!("  read failed: {e}");
                    container_error_code(arc, &e)
                }
            };
            if code == 0 {
                dlog!("  ok in {} ms", started.elapsed().as_millis());
                if op == PK_EXTRACT {
                    if let Some(d) = &dest {
                        source_stream_raw(arc, index, &item.display, d);
                    }
                }
            } else {
                dlog!("  -> {}", crate::code_name(code));
                if code != E_EABORTED {
                    log::on_error();
                }
            }
            code
        }
    }
}

/// Whether a listing entry is a `.uasset`/`.umap` package.
fn is_package_name(display: &str) -> bool {
    let d = display.to_ascii_lowercase();
    d.ends_with(".uasset") || d.ends_with(".umap")
}

/// N7: `:uetoc.source` on an extracted raw file (when the scope and volume allow).
fn source_stream_raw(arc: &mut Archive, index: u32, display: &str, dest: &Path) {
    use crate::source_stream as ss;
    let Ok(info) = arc.container.chunk(index) else { return };
    let is_package = info.kind == iostore::ChunkKind::ExportBundleData && is_package_name(display);
    if !ss::wanted(arc.source_stream, dest, &std::env::temp_dir(), is_package) {
        return;
    }
    if arc.source_engine_range.is_none() {
        let toc = retoc::EIoStoreTocVersion::from_repr(arc.container.version().0);
        let header = arc
            .container
            .chunks()
            .find(|c| c.kind == iostore::ChunkKind::ContainerHeader)
            .and_then(|c| arc.container.read_chunk(c.index).ok())
            .and_then(|d| retoc::container_header::FIoContainerHeader::deserialize(&mut std::io::Cursor::new(d), None).ok())
            .map(|h| h.version);
        let names: Vec<&str> = toc.map(|t| crate::legacy::candidates(t, header)).unwrap_or_default().into_iter().map(crate::legacy::version_ini_name).collect();
        arc.source_engine_range = Some(ss::range_text(&names));
    }
    let src = ss::SourceInfo {
        legacy: false,
        container: arc.container.utoc_path().to_path_buf(),
        paks: arc.global.as_deref().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_default(),
        entry: display.to_string(),
        chunk: Some(info.id.hex()),
        toc_hash: Some(hex(&info.meta.hash)),
        engine: None,
        engine_range: arc.source_engine_range.clone().flatten(),
    };
    ss::write(arc.source_stream, dest, &src, is_package);
}

/// Writes a file produced in memory (`dest` None = test).
fn write_bytes(dest: Option<&Path>, display: &str, bytes: &[u8]) -> c_int {
    let Some(d) = dest else {
        let mut name = win::wide(display);
        return if progress::report(&mut name, bytes.len() as u64) { 0 } else { E_EABORTED };
    };
    if let Some(parent) = d.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(d, bytes) {
        Ok(()) => {
            let mut name = win::wide(&d.to_string_lossy());
            if progress::report(&mut name, bytes.len() as u64) {
                0
            } else {
                E_EABORTED
            }
        }
        Err(e) => {
            dlog!("  write failed: {e}");
            let _ = std::fs::remove_file(d);
            if e.kind() == std::io::ErrorKind::NotFound || e.kind() == std::io::ErrorKind::PermissionDenied {
                E_ECREATE
            } else {
                E_EWRITE
            }
        }
    }
}

/// Extract/test of a `$legacy` file (SPEC §7).
fn process_legacy(arc: &mut Archive, op: c_int, dest: Option<PathBuf>, display: &str, kind: crate::legacy::LegacyKind) -> c_int {
    use crate::legacy::{LegacyError, LegacyKind, Session};
    if arc.legacy_declined {
        return E_EABORTED;
    }
    if op == PK_EXTRACT && dest.is_none() {
        return E_ECREATE;
    }
    if arc.legacy.is_none() {
        if arc.legacy_error.is_some() {
            return E_BAD_DATA;
        }
        let Some(global) = arc.global.clone() else { return E_BAD_DATA };
        let utoc = arc.container.utoc_path().to_path_buf();
        match Session::open(&global, &utoc, arc.legacy_guess_script_types) {
            Ok(s) => arc.legacy = Some(s),
            Err(e) => {
                let msg = match e {
                    LegacyError::Failed(m) | LegacyError::Oodle(m) => m,
                    LegacyError::Cancelled => String::new(),
                };
                dlog!("  legacy view unavailable: {msg}");
                show_error(&format!("{}\n\nThe legacy view can't be used: {msg}", arc.path.display()));
                arc.legacy_error = Some(msg);
                log::on_error();
                return E_BAD_DATA;
            }
        }
    }
    let started = Instant::now();
    let ini = arc.legacy_version;
    let session = arc.legacy.as_mut().unwrap();
    let container = &arc.container;
    let owned;
    let bytes: Result<&[u8], LegacyError> = match &kind {
        LegacyKind::Package { chunk, part, path } => session.package_part(container, *chunk, path, *part, ini),
        LegacyKind::ShaderLibrary { chunk, asset_info } => session.shader_part(container, *chunk, *asset_info),
        LegacyKind::ScriptObjects => match session.script_objects() {
            Ok(v) => {
                owned = v;
                Ok(&owned)
            }
            Err(e) => Err(e),
        },
    };
    let code = match bytes {
        Ok(b) => {
            let code = write_bytes(if op == PK_TEST { None } else { dest.as_deref() }, display, b);
            if let (0, PK_EXTRACT, Some(d)) = (code, op, dest.as_deref()) {
                // N7: every $legacy file under All; packages (.uasset/.umap) under View
                let (chunk, is_package) = match &kind {
                    LegacyKind::Package { chunk, part, .. } => (Some(*chunk), *part == crate::legacy::Part::Asset),
                    LegacyKind::ShaderLibrary { chunk, .. } => (Some(*chunk), false),
                    LegacyKind::ScriptObjects => (None, false),
                };
                if crate::source_stream::wanted(arc.source_stream, d, &std::env::temp_dir(), is_package) {
                    let info = chunk.and_then(|c| container.chunk(c).ok());
                    let names: Vec<&str> = session.engine_candidates().into_iter().map(crate::legacy::version_ini_name).collect();
                    let engine = match &kind {
                        LegacyKind::Package { chunk, .. } => session.version_used(*chunk).map(|v| crate::legacy::version_ini_name(v).to_string()),
                        _ => None,
                    };
                    let src = crate::source_stream::SourceInfo {
                        legacy: true,
                        container: container.utoc_path().to_path_buf(),
                        paks: arc.global.as_deref().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_default(),
                        entry: display.to_string(),
                        chunk: info.as_ref().map(|i| i.id.hex()),
                        toc_hash: info.as_ref().map(|i| hex(&i.meta.hash)),
                        engine,
                        engine_range: crate::source_stream::range_text(&names),
                    };
                    crate::source_stream::write(arc.source_stream, d, &src, is_package);
                }
            }
            code
        }
        Err(LegacyError::Cancelled) => {
            arc.legacy_declined = true;
            E_EABORTED
        }
        Err(LegacyError::Oodle(detail)) => {
            if !arc.oodle_warned {
                arc.oodle_warned = true;
                show_warning(&oodle_missing_text(&arc.path, &detail));
            }
            E_NOT_SUPPORTED
        }
        Err(LegacyError::Failed(msg)) => {
            dlog!("  conversion failed: {msg}");
            E_BAD_DATA
        }
    };
    if code == 0 {
        dlog!("  ok in {} ms", started.elapsed().as_millis());
    } else {
        dlog!("  -> {}", crate::code_name(code));
        if code != E_EABORTED {
            log::on_error();
        }
    }
    code
}

fn container_error_code(arc: &mut Archive, e: &IoError) -> c_int {
    match e {
        IoError::OodleUnavailable(detail) => {
            if !arc.oodle_warned {
                arc.oodle_warned = true;
                show_warning(&oodle_missing_text(&arc.path, detail));
            }
            E_NOT_SUPPORTED
        }
        IoError::MissingPartition(p) => {
            if !arc.partition_warned {
                arc.partition_warned = true;
                show_warning(&format!("{}\n\nThe data file {} is missing.", arc.path.display(), p.display()));
            }
            E_EREAD
        }
        IoError::OnDemand(_) => {
            if first_time(format!("ondemand|{}", arc.path.display())) {
                show_warning(&format!(
                    "{}\n\nThis is an on-demand container: its data is downloaded by the game and isn't stored in the .ucas. Extracting isn't supported.",
                    arc.path.display()
                ));
            }
            E_NOT_SUPPORTED
        }
        IoError::KeyRequired(_) => E_NOT_SUPPORTED,
        IoError::UnknownCompression(name) => {
            if first_time(format!("method|{}|{name}", arc.path.display())) {
                show_warning(&format!("{}\n\nUnknown compression method \"{name}\".", arc.path.display()));
            }
            E_NOT_SUPPORTED
        }
        IoError::Io(_) => E_EREAD,
        _ => E_BAD_DATA,
    }
}

pub fn join_dest(path: Option<String>, name: Option<String>) -> Option<PathBuf> {
    let name = name?;
    match path {
        Some(dir) if !dir.is_empty() => Some(Path::new(&dir).join(name)),
        _ => Some(PathBuf::from(name)),
    }
}
