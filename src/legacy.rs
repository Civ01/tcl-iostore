//! Legacy view (`$legacy`, SPEC §6a): Zen packages converted to legacy cooked files with
//! retoc's `to-legacy` code, reading through the `iostore` crate.

use std::collections::HashMap;
use std::io::Cursor;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use iostore::{ChunkKind, Container};
use retoc::asset_conversion::{FZenPackageContext, build_legacy_without_bulk_data};
use retoc::container_header::EIoContainerHeaderVersion;
use retoc::iostore::{ChunkDataSource, IoStoreContainer, IoStoreTrait};
use retoc::logging::{Log, LogBackend, LogLevel};
use retoc::version::EngineVersion;
use retoc::zen::ZenScriptCellsStore;
use retoc::{Config, EIoStoreTocVersion, FIoChunkId, FIoChunkIdRaw, FPackageId, FileWriterTrait, UEPath};

use crate::{config, dialog, dlog, keys};

/// Hidden folder at the panel root with the legacy view.
pub const LEGACY_DIR: &str = "$legacy";
pub const SCRIPT_OBJECTS_NAME: &str = "scriptobjects.bin";

// ---------------------------------------------------------------------------
// Listing

/// One file of a converted package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Part {
    /// `.uasset` / `.umap`
    Asset,
    /// `.uexp`
    Exports,
    /// `.ubulk`
    Bulk,
    /// `.uptnl`
    OptionalBulk,
    /// `.m.ubulk`
    MemoryMappedBulk,
}

impl Part {
    fn of_output(path: &str) -> Option<Part> {
        let p = path.to_ascii_lowercase();
        Some(if p.ends_with(".uexp") {
            Part::Exports
        } else if p.ends_with(".m.ubulk") {
            Part::MemoryMappedBulk
        } else if p.ends_with(".ubulk") {
            Part::Bulk
        } else if p.ends_with(".uptnl") {
            Part::OptionalBulk
        } else if p.ends_with(".uasset") || p.ends_with(".umap") {
            Part::Asset
        } else {
            return None;
        })
    }
    fn of_bulk(kind: ChunkKind) -> Option<Part> {
        match kind {
            ChunkKind::BulkData => Some(Part::Bulk),
            ChunkKind::OptionalBulkData => Some(Part::OptionalBulk),
            ChunkKind::MemoryMappedBulkData => Some(Part::MemoryMappedBulk),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyKind {
    /// `.uasset`/`.umap` or `.uexp` of the package whose `ExportBundleData` chunk is `chunk`;
    /// `path` is the package's directory-index path (`…/X.uasset`). Its bulk files are listed
    /// as raw chunks (retoc copies them unchanged).
    Package { chunk: u32, part: Part, path: String },
    /// Rebuilt shader library (`asset_info` = its `ShaderAssetInfo-*.assetinfo.json`).
    ShaderLibrary { chunk: u32, asset_info: bool },
    /// `scriptobjects.bin` (global container, UE 5.0+).
    ScriptObjects,
}

/// A planned `$legacy` entry: path relative to the container (directory-index form, `/`),
/// what produces it, and the size shown before conversion.
pub enum Planned {
    Legacy { rel: String, kind: LegacyKind, size: u64 },
    /// Copied as is from chunk `index`.
    Raw { rel: String, index: u32, pack: u64, size: u64 },
}

fn chunk_index_field(id: &iostore::ChunkId) -> u16 {
    u16::from_le_bytes([id.0[8], id.0[9]])
}

/// The legacy view's files, in directory-index order with each package's files together.
pub fn plan(c: &Container, files: &[(String, u32)]) -> Vec<Planned> {
    // bulk chunks of the packages in this container
    // (path, TOC index, packed, size)
    let mut bulk: HashMap<u64, Vec<(String, u32, u64, u64)>> = HashMap::new();
    let mut packages: std::collections::HashSet<u64> = std::collections::HashSet::new();
    for (path, i) in files {
        let Ok(info) = c.chunk(*i) else { continue };
        if chunk_index_field(&info.id) != 0 {
            continue;
        }
        if info.kind == ChunkKind::ExportBundleData {
            packages.insert(info.id.id64());
        } else if Part::of_bulk(info.kind).is_some() {
            bulk.entry(info.id.id64()).or_default().push((path.clone(), *i, info.packed_size, info.size));
        }
    }
    let mut out = Vec::new();
    for (path, i) in files {
        let Ok(info) = c.chunk(*i) else { continue };
        let id = info.id.id64();
        let main = chunk_index_field(&info.id) == 0;
        let lower = path.to_ascii_lowercase();
        if main && info.kind == ChunkKind::ExportBundleData && (lower.ends_with(".uasset") || lower.ends_with(".umap")) {
            out.push(Planned::Legacy { rel: path.clone(), kind: LegacyKind::Package { chunk: *i, part: Part::Asset, path: path.clone() }, size: info.size });
            let stem = &path[..path.rfind('.').unwrap_or(path.len())];
            out.push(Planned::Legacy { rel: format!("{stem}.uexp"), kind: LegacyKind::Package { chunk: *i, part: Part::Exports, path: path.clone() }, size: 0 });
            // bulk files are the bulk chunks unchanged: streamed like raw files
            for (bpath, index, pack, size) in bulk.get(&id).into_iter().flatten() {
                out.push(Planned::Raw { rel: bpath.clone(), index: *index, pack: *pack, size: *size });
            }
        } else if main && Part::of_bulk(info.kind).is_some() && packages.contains(&id) {
            // listed with its package
        } else if info.kind == ChunkKind::ShaderCodeLibrary && lower.ends_with(".ushaderbytecode") {
            match retoc::shader_library::get_shader_asset_info_filename_from_library_filename(path) {
                Ok(json) => {
                    out.push(Planned::Legacy { rel: path.clone(), kind: LegacyKind::ShaderLibrary { chunk: *i, asset_info: false }, size: info.size });
                    out.push(Planned::Legacy { rel: json, kind: LegacyKind::ShaderLibrary { chunk: *i, asset_info: true }, size: 0 });
                }
                Err(_) => out.push(Planned::Raw { rel: path.clone(), index: *i, pack: info.packed_size, size: info.size }),
            }
        } else {
            out.push(Planned::Raw { rel: path.clone(), index: *i, pack: info.packed_size, size: info.size });
        }
    }
    if c.version().0 >= 5 && c.has_script_objects() {
        let size = c.chunks().find(|i| i.kind == ChunkKind::ScriptObjects).map(|i| i.size).unwrap_or(0);
        out.push(Planned::Legacy { rel: SCRIPT_OBJECTS_NAME.into(), kind: LegacyKind::ScriptObjects, size });
    }
    out
}

// ---------------------------------------------------------------------------
// Engine versions

/// Versions offered in the ini and the prompt: (value, ini name, label).
pub const VERSIONS: &[(EngineVersion, &str, &str)] = &[
    (EngineVersion::UE4_27, "UE4_27", "UE 4.27"),
    (EngineVersion::UE5_0, "UE5_0", "UE 5.0"),
    (EngineVersion::UE5_1, "UE5_1", "UE 5.1"),
    (EngineVersion::UE5_2, "UE5_2", "UE 5.2"),
    (EngineVersion::UE5_3, "UE5_3", "UE 5.3"),
    (EngineVersion::UE5_4, "UE5_4", "UE 5.4"),
    (EngineVersion::UE5_5, "UE5_5", "UE 5.5"),
    (EngineVersion::UE5_6, "UE5_6", "UE 5.6"),
    (EngineVersion::UE5_7, "UE5_7", "UE 5.7"),
    (EngineVersion::UE5_8, "UE5_8", "UE 5.8"),
];

pub fn version_ini_name(v: EngineVersion) -> &'static str {
    VERSIONS.iter().find(|x| x.0 == v).map(|x| x.1).unwrap_or("Auto")
}

fn version_label(v: EngineVersion) -> &'static str {
    VERSIONS.iter().find(|x| x.0 == v).map(|x| x.2).unwrap_or("?")
}

/// `LegacyEngineVersion` value: `Ok(None)` = Auto.
pub fn parse_version(s: &str) -> Result<Option<EngineVersion>, ()> {
    let n: String = s.trim().to_ascii_uppercase().chars().filter(|c| !matches!(c, ' ' | '_' | '.')).collect();
    if n.is_empty() || n == "AUTO" {
        return Ok(None);
    }
    let n = n.strip_prefix("UE").unwrap_or(&n);
    VERSIONS.iter().find(|x| x.1.replace('_', "").strip_prefix("UE") == Some(n)).map(|x| Some(x.0)).ok_or(())
}

/// Engine versions that write this TOC + container-header version.
pub fn candidates(toc: EIoStoreTocVersion, header: Option<EIoContainerHeaderVersion>) -> Vec<EngineVersion> {
    VERSIONS.iter().map(|x| x.0).filter(|v| v.toc_version() == toc && Some(v.container_header_version()) == header).collect()
}

/// Narrows the candidates with the shader hash size seen in the container headers (8 bytes
/// only from UE 5.8 on). 5.7 and 5.8 share the object version (1018) but not the legacy
/// import layout (5.8 writes `PackageName` in cooked imports), so both stay when it's unknown.
pub fn refine(cands: Vec<EngineVersion>, shader_hash_size: Option<usize>) -> Vec<EngineVersion> {
    let mut c = cands;
    if let Some(size) = shader_hash_size {
        let fit: Vec<_> = c.iter().copied().filter(|v| v.shader_hash_size() == size).collect();
        if !fit.is_empty() {
            c = fit;
        }
    }
    c
}

fn is_5_8(v: Option<EngineVersion>) -> bool {
    v.is_some_and(|v| v >= EngineVersion::UE5_8)
}

/// Custom versions whose values were bumped between UE 5.7 and 5.8, with their 5.7 value
/// (measured on the 5.7/5.8 samples: 5.7 = 61/121/225, 5.8 = 68/123/268; 5.6 = 56/121/207).
const MARKERS_5_7: [([u32; 4], i32); 3] = [
    ([0xD89B5E42, 0x24BD4D46, 0x8412ACA8, 0xDF641779], 61),  // FUE5ReleaseStreamObjectVersion
    ([0x697DD581, 0xE64F41AB, 0xAA4A51EC, 0xBEB7B628], 121), // FUE5MainStreamObjectVersion
    ([0x601D1886, 0xAC644F84, 0xAA16D3DE, 0x0DEAC7D6], 225), // FFortniteMainBranchObjectVersion
];

/// Whether a versioned package was saved by UE 5.8 or later (`None`: none of the markers).
pub fn saved_by_5_8(custom_versions: &[(retoc::FGuid, i32)]) -> Option<bool> {
    for (words, v57) in MARKERS_5_7 {
        let mut b = [0u8; 16];
        for (i, w) in words.iter().enumerate() {
            b[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
        }
        let guid = retoc::FGuid::from_bytes(b);
        if let Some((_, v)) = custom_versions.iter().find(|(k, _)| *k == guid) {
            return Some(*v > v57);
        }
    }
    None
}

/// retoc's heuristic picks one version by itself (5.1/5.2 are told apart per package).
fn heuristic_suffices(c: &[EngineVersion]) -> bool {
    c.len() == 1 || c == [EngineVersion::UE5_1, EngineVersion::UE5_2]
}

/// The pick that holds until another container is opened ("don't ask again").
static SESSION_PICK: Mutex<Option<(PathBuf, EngineVersion)>> = Mutex::new(None);
static PROMPT: Mutex<()> = Mutex::new(());

/// Called on every OpenArchive: a different container ends the "don't ask again" pick.
pub fn note_opened(utoc: &Path) {
    let mut p = SESSION_PICK.lock().unwrap_or_else(|e| e.into_inner());
    if p.as_ref().is_some_and(|(u, _)| u != utoc) {
        dlog!("  legacy engine version pick for {} dropped", p.as_ref().unwrap().0.display());
        *p = None;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    /// Not needed yet (versioned packages only so far) or not settled.
    Undecided,
    /// `None` = let retoc's heuristic decide.
    Use(Option<EngineVersion>),
}

/// Version to use for unversioned packages, without asking. `None` = must ask.
fn decide_quietly(opts_version: Option<EngineVersion>, utoc: &Path, cands: &[EngineVersion], header: Option<EIoContainerHeaderVersion>) -> Option<Decision> {
    if let Some(v) = opts_version {
        if Some(v.container_header_version()) == header {
            return Some(Decision::Use(Some(v)));
        }
        dlog!("  LegacyEngineVersion={} doesn't fit this container (header {header:?}); using Auto", version_ini_name(v));
    }
    if let Some((u, v)) = SESSION_PICK.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if u == utoc {
            return Some(Decision::Use(Some(*v)));
        }
    }
    if heuristic_suffices(cands) {
        // retoc's heuristic finds the package version; 5.8 must still be named for the imports
        return Some(Decision::Use(if cands.len() == 1 && is_5_8(Some(cands[0])) { Some(cands[0]) } else { None }));
    }
    None
}

// ---------------------------------------------------------------------------
// Store: every container near the global, read through `iostore`

struct Source(Container);
impl ChunkDataSource for Source {
    fn read_chunk(&self, toc_index: u32) -> anyhow::Result<Vec<u8>> {
        Ok(self.0.read_chunk(toc_index)?)
    }
}

struct Store {
    global: PathBuf,
    fingerprint: Vec<(PathBuf, u64, Option<SystemTime>)>,
    backend: Box<dyn IoStoreTrait>,
    toc: EIoStoreTocVersion,
    header: Option<EIoContainerHeaderVersion>,
    /// Shader map hash size in the package store entries (`None`: no package has shader maps).
    shader_hash_size: Option<usize>,
}

static STORE: Mutex<Option<Arc<Store>>> = Mutex::new(None);

fn find_utocs(dir: &Path, out: &mut Vec<PathBuf>, depth: u32) {
    if depth > 16 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut v: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    v.sort();
    for p in v {
        if p.is_dir() {
            find_utocs(&p, out, depth + 1);
        } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("utoc")) {
            out.push(p);
        }
    }
}

fn fingerprint(utocs: &[PathBuf]) -> Vec<(PathBuf, u64, Option<SystemTime>)> {
    utocs
        .iter()
        .map(|p| {
            let m = std::fs::metadata(p).ok();
            (p.clone(), m.as_ref().map(|m| m.len()).unwrap_or(0), m.and_then(|m| m.modified().ok()))
        })
        .collect()
}

/// Opens `utoc` for the store: key from the remembered keys (no prompts), retoc's TOC.
fn open_for_store(utoc: &Path) -> Result<IoStoreContainer, String> {
    let mut c = Container::open(utoc, crate::profile::profile_for(utoc)).map_err(|e| e.to_string())?;
    let mut config = Config::default();
    if c.is_encrypted() {
        let folder = utoc.parent().unwrap_or(Path::new("")).to_path_buf();
        let guid = c.header().key_guid.to_string();
        let key = keys::candidates(&folder, &guid).into_iter().map(|(k, _, _)| k).find(|k| c.set_key(k.to_iostore()).is_ok()).ok_or("encrypted, no known key")?;
        let hex: String = key.0.iter().map(|b| format!("{b:02x}")).collect();
        let rkey: retoc::AesKey = hex.parse().map_err(|e: anyhow::Error| e.to_string())?;
        config.aes_keys.insert(retoc::FGuid::from_bytes(c.header().key_guid.0), rkey);
    }
    let source: Arc<dyn ChunkDataSource> = Arc::new(Source(c));
    let config = Arc::new(config);
    match std::panic::catch_unwind(AssertUnwindSafe(|| IoStoreContainer::open_with_source(utoc, config, source))) {
        Ok(Ok(c)) => Ok(c),
        Ok(Err(e)) => Err(format!("{e:#}")),
        Err(_) => Err("retoc panicked while reading the TOC".into()),
    }
}

fn build_store(global: &Path, current: &Path) -> Result<Store, String> {
    let root = global.parent().unwrap_or(Path::new("."));
    let mut utocs = Vec::new();
    find_utocs(root, &mut utocs, 0);
    if !utocs.iter().any(|p| p == current) {
        utocs.push(current.to_path_buf());
    }
    let fp = fingerprint(&utocs);
    let started = std::time::Instant::now();
    let cur = open_for_store(current).map_err(|e| format!("{}: {e}", current.display()))?;
    let toc = cur.container_file_version().ok_or("no TOC version")?;
    let header = cur.container_header_version();
    let mut list: Vec<Box<dyn IoStoreTrait>> = vec![Box::new(cur)];
    let mut skipped = 0;
    for p in utocs.iter().filter(|p| *p != current) {
        match open_for_store(p) {
            Ok(c) if c.container_file_version() != Some(toc) => {
                dlog!("  legacy store: skipped {} (TOC {:?}, container {toc:?})", p.display(), c.container_file_version());
                skipped += 1;
            }
            Ok(c) if c.container_header_version().is_some() && c.container_header_version() != header => {
                dlog!("  legacy store: skipped {} (header {:?}, container {header:?})", p.display(), c.container_header_version());
                skipped += 1;
            }
            Ok(c) => list.push(Box::new(c)),
            Err(e) => {
                dlog!("  legacy store: skipped {}: {e}", p.display());
                skipped += 1;
            }
        }
    }
    let n = list.len();
    let backend = retoc::iostore::backend_from_containers(list).map_err(|e| format!("{e:#}"))?;
    dlog!("  legacy store: {n} containers ({skipped} skipped) under {} in {} ms", root.display(), started.elapsed().as_millis());
    let shader_hash_size = backend.packages().find_map(|p| p.container().package_store_entry(p.id()).and_then(|e| e.shader_map_hashes.first().map(|h| h.len())));
    dlog!("  legacy store: shader hash size {shader_hash_size:?}");
    Ok(Store { global: global.to_path_buf(), fingerprint: fp, backend, toc, header, shader_hash_size })
}

fn store_for(global: &Path, current: &Path) -> Result<Arc<Store>, String> {
    let mut slot = STORE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = slot.as_ref() {
        if s.global == global && s.fingerprint.iter().any(|(p, _, _)| p == current) {
            let paths: Vec<PathBuf> = s.fingerprint.iter().map(|x| x.0.clone()).collect();
            let mut now = Vec::new();
            find_utocs(global.parent().unwrap_or(Path::new(".")), &mut now, 0);
            if !now.iter().any(|p| p == current) {
                now.push(current.to_path_buf());
            }
            if now == paths && fingerprint(&now) == s.fingerprint {
                return Ok(s.clone());
            }
            dlog!("  legacy store: containers changed, reloading");
        }
    }
    // one store at a time: a whole game's TOCs can be large
    *slot = None;
    let s = Arc::new(build_store(global, current)?);
    *slot = Some(s.clone());
    Ok(s)
}

/// Drops the cached store (tests).
#[cfg(test)]
pub fn reset() {
    *STORE.lock().unwrap() = None;
    *SESSION_PICK.lock().unwrap() = None;
}

// ---------------------------------------------------------------------------
// Session (one per archive handle)

struct ToPluginLog;
impl LogBackend for ToPluginLog {
    fn write_message(&self, level: LogLevel, msg: &str) {
        crate::dlog!("  retoc {}{msg}", level);
    }
}

struct Collect(Mutex<Vec<(String, Vec<u8>)>>);
impl FileWriterTrait for Collect {
    fn write_file(&self, path: String, _allow_compress: bool, data: Vec<u8>) -> anyhow::Result<()> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).push((path, data));
        Ok(())
    }
}

#[derive(Debug)]
pub enum LegacyError {
    Cancelled,
    Oodle(String),
    Failed(String),
}

fn convert_error(e: anyhow::Error) -> LegacyError {
    for cause in e.chain() {
        if let Some(iostore::Error::OodleUnavailable(d)) = cause.downcast_ref::<iostore::Error>() {
            return LegacyError::Oodle(d.clone());
        }
    }
    LegacyError::Failed(format!("{e:#}"))
}

fn guarded<T>(f: impl FnOnce() -> anyhow::Result<T>) -> Result<T, LegacyError> {
    match std::panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(convert_error(e)),
        Err(p) => {
            let msg = p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
            Err(LegacyError::Failed(format!("conversion panicked: {msg}")))
        }
    }
}

pub struct Session {
    // declared first: dropped before the store and log it borrows
    ctx: Option<((Option<EngineVersion>, bool), FZenPackageContext<'static>)>,
    log: Box<Log>,
    cells: Arc<ZenScriptCellsStore>,
    store: Arc<Store>,
    utoc: PathBuf,
    decision: Decision,
    /// From a versioned package's custom versions: saved by 5.8+ or not.
    known_5_8: Option<bool>,
    package: Option<(u32, HashMap<Part, Vec<u8>>)>,
    shader: Option<(u32, Vec<u8>, Vec<u8>)>,
}

impl Session {
    pub fn open(global: &Path, current: &Path) -> Result<Session, LegacyError> {
        let store = store_for(global, current).map_err(LegacyError::Failed)?;
        Ok(Session {
            ctx: None,
            log: Box::new(Log::new(LogLevel::Info, Arc::new(ToPluginLog))),
            cells: retoc::build_verse_cell_store(&Vec::new()),
            store,
            utoc: current.to_path_buf(),
            decision: Decision::Undecided,
            known_5_8: None,
            package: None,
            shader: None,
        })
    }

    /// `version`: fallback package version (`None`: retoc's heuristic); `is_5_8`: write UE 5.8's
    /// cooked import layout.
    fn context(&mut self, version: Option<EngineVersion>, is_5_8: bool) -> &FZenPackageContext<'static> {
        if self.ctx.as_ref().is_none_or(|(k, _)| *k != (version, is_5_8)) {
            self.ctx = None;
            // SAFETY: the backend lives in `self.store` (an Arc, never replaced) and the log in
            // `self.log` (a Box, never replaced); `ctx` is declared first, so it is dropped
            // before both.
            let backend: &'static dyn IoStoreTrait = unsafe { &*(self.store.backend.as_ref() as *const dyn IoStoreTrait) };
            let log: &'static Log = unsafe { &*(self.log.as_ref() as *const Log) };
            let fallback = version.map(|v| v.package_file_version());
            dlog!("  legacy: package context, engine version {}, 5.8 imports {is_5_8}", version.map(version_ini_name).unwrap_or("Auto"));
            let engine = if is_5_8 { Some(EngineVersion::UE5_8) } else { None };
            let ctx = FZenPackageContext::create(backend, fallback, log, Some(self.cells.clone())).with_engine_version(engine);
            self.ctx = Some(((version, is_5_8), ctx));
        }
        &self.ctx.as_ref().unwrap().1
    }

    /// Fallback package version (`None`: retoc's heuristic) and whether to write UE 5.8's import
    /// layout, for this package. Asks only when neither the container nor the package tells.
    fn version_for(&mut self, c: &Container, chunk: u32, ini: Option<EngineVersion>) -> Result<(Option<EngineVersion>, bool), LegacyError> {
        if let Decision::Use(v) = self.decision {
            return Ok((v, is_5_8(v)));
        }
        let mut cands = refine(candidates(self.store.toc, self.store.header), self.store.shader_hash_size);
        if let Some(k) = self.known_5_8 {
            let narrowed: Vec<_> = cands.iter().copied().filter(|v| (*v >= EngineVersion::UE5_8) == k).collect();
            if !narrowed.is_empty() {
                cands = narrowed;
            }
        }
        if let Some(d) = decide_quietly(ini, &self.utoc, &cands, self.store.header) {
            self.decision = d;
            let Decision::Use(v) = d else { unreachable!() };
            return Ok((v, is_5_8(v)));
        }
        let mixed = cands.iter().any(|v| *v >= EngineVersion::UE5_8) && cands.iter().any(|v| *v < EngineVersion::UE5_8);
        if let Some(header) = self.store.header.filter(|h| *h > EIoContainerHeaderVersion::Initial) {
            if let Some(custom) = versioning_info(c, chunk, header) {
                if !mixed {
                    // the package's own versions cover the package version; layout is the same for all
                    return Ok((None, cands.first().is_some_and(|v| *v >= EngineVersion::UE5_8)));
                }
                if let Some(k) = saved_by_5_8(&custom) {
                    dlog!("  legacy: versioned package saved by {} (custom versions)", if k { "UE 5.8+" } else { "UE 5.7 or older" });
                    self.known_5_8 = Some(k);
                    return self.version_for(c, chunk, ini);
                }
            }
        }
        let v = ask(&self.utoc, &cands, ini)?;
        self.decision = Decision::Use(Some(v));
        Ok((Some(v), is_5_8(Some(v))))
    }

    /// File `part` of the package at `chunk` (converted once, then served from memory).
    pub fn package_part(&mut self, c: &Container, chunk: u32, path: &str, part: Part, ini: Option<EngineVersion>) -> Result<&[u8], LegacyError> {
        if self.package.as_ref().is_none_or(|(i, _)| *i != chunk) {
            self.package = None;
            let info = c.chunk(chunk).map_err(|e| LegacyError::Failed(e.to_string()))?;
            let (version, layout_5_8) = self.version_for(c, chunk, ini)?;
            let id = FPackageId(info.id.id64());
            let out = Collect(Mutex::new(Vec::new()));
            let started = std::time::Instant::now();
            let ctx = self.context(version, layout_5_8);
            guarded(|| build_legacy_without_bulk_data(ctx, id, UEPath::new(path), &out))?;
            let mut files = HashMap::new();
            for (p, data) in out.0.into_inner().unwrap_or_else(|e| e.into_inner()) {
                match Part::of_output(&p) {
                    Some(k) => {
                        files.insert(k, data);
                    }
                    None => dlog!("  legacy: unexpected output {p}"),
                }
            }
            dlog!("  legacy: converted {path} in {} ms ({} files)", started.elapsed().as_millis(), files.len());
            self.package = Some((chunk, files));
        }
        let files = &self.package.as_ref().unwrap().1;
        files.get(&part).map(Vec::as_slice).ok_or_else(|| LegacyError::Failed(format!("the conversion produced no {part:?} file")))
    }

    pub fn shader_part(&mut self, c: &Container, chunk: u32, asset_info: bool) -> Result<&[u8], LegacyError> {
        if self.shader.as_ref().is_none_or(|(i, _, _)| *i != chunk) {
            self.shader = None;
            let info = c.chunk(chunk).map_err(|e| LegacyError::Failed(e.to_string()))?;
            let id = FIoChunkId::from_raw(FIoChunkIdRaw { id: info.id.0 }, self.store.toc);
            let backend = self.store.backend.as_ref();
            let log = self.log.as_ref();
            let started = std::time::Instant::now();
            let (lib, json) = guarded(|| retoc::shader_library::rebuild_shader_library_from_io_store(backend, id, log, true))?;
            dlog!("  legacy: shader library rebuilt in {} ms ({} + {} bytes)", started.elapsed().as_millis(), lib.len(), json.len());
            self.shader = Some((chunk, lib, json));
        }
        let (_, lib, json) = self.shader.as_ref().unwrap();
        Ok(if asset_info { json } else { lib })
    }

    pub fn script_objects(&mut self) -> Result<Vec<u8>, LegacyError> {
        let backend = self.store.backend.as_ref();
        guarded(|| {
            let so = backend.load_script_objects()?;
            let mut buf = Vec::new();
            so.serialize_new(&mut Cursor::new(&mut buf))?;
            Ok(buf)
        })
    }
}

/// Custom versions of a versioned package (`None`: unversioned or unreadable).
fn versioning_info(c: &Container, chunk: u32, header: EIoContainerHeaderVersion) -> Option<Vec<(retoc::FGuid, i32)>> {
    let mut r = c.open_chunk(chunk).ok()?;
    let first = r.next_piece().ok()??;
    let info = std::panic::catch_unwind(AssertUnwindSafe(|| retoc::zen::read_zen_package_versioning_info(&mut Cursor::new(&first), header))).ok()?.ok()??;
    Some(info.custom_versions.iter().map(|v| (v.key, v.version)).collect())
}

fn ask(utoc: &Path, cands: &[EngineVersion], ini: Option<EngineVersion>) -> Result<EngineVersion, LegacyError> {
    let _one = PROMPT.lock().unwrap_or_else(|e| e.into_inner());
    // another thread may have picked meanwhile
    if let Some((u, v)) = SESSION_PICK.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if u == utoc {
            return Ok(*v);
        }
    }
    let list: Vec<EngineVersion> = if cands.is_empty() { VERSIONS.iter().map(|x| x.0).collect() } else { cands.to_vec() };
    let selected = ini.and_then(|v| list.iter().position(|x| *x == v)).unwrap_or(list.len() - 1);
    let labels: Vec<&str> = list.iter().map(|v| version_label(*v)).collect();
    dlog!("  legacy: asking for the engine version ({labels:?})");
    let Some(a) = dialog::prompt_version(&dialog::VersionPrompt { container: utoc, choices: &labels, selected, known: !cands.is_empty() }) else {
        dlog!("  legacy: version prompt cancelled");
        return Err(LegacyError::Cancelled);
    };
    let v = list[a.index.min(list.len() - 1)];
    dlog!("  legacy: picked {} (remember={}, session={})", version_ini_name(v), a.remember, a.session);
    if a.remember {
        if let Err(e) = config::set_option("LegacyEngineVersion", version_ini_name(v)) {
            dlog!("  could not save LegacyEngineVersion: {e}");
            crate::show_warning(&format!("The engine version could not be saved to {}:\n{e}", config::INI_NAME));
        }
    }
    if a.session {
        *SESSION_PICK.lock().unwrap_or_else(|e| e.into_inner()) = Some((utoc.to_path_buf(), v));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("Auto"), Ok(None));
        assert_eq!(parse_version(""), Ok(None));
        assert_eq!(parse_version("UE5_7"), Ok(Some(EngineVersion::UE5_7)));
        assert_eq!(parse_version("5.8"), Ok(Some(EngineVersion::UE5_8)));
        assert_eq!(parse_version("UE5_8"), Ok(Some(EngineVersion::UE5_8)));
        assert_eq!(parse_version("ue4.27"), Ok(Some(EngineVersion::UE4_27)));
        assert!(parse_version("UE6").is_err());
        use EIoContainerHeaderVersion as H;
        use EIoStoreTocVersion as T;
        assert_eq!(candidates(T::PartitionSize, Some(H::Initial)), [EngineVersion::UE4_27]);
        assert!(heuristic_suffices(&candidates(T::PerfectHashWithOverflow, Some(H::OptionalSegmentPackages))));
        assert_eq!(candidates(T::OnDemandMetaData, Some(H::NoExportInfo)), [EngineVersion::UE5_4]);
        let late = candidates(T::ReplaceIoChunkHashWithIoHash, Some(H::SoftPackageReferencesOffset));
        assert_eq!(late, [EngineVersion::UE5_6, EngineVersion::UE5_7, EngineVersion::UE5_8]);
        // no shader maps: all three (5.8 differs from 5.7 in the legacy import layout)
        assert_eq!(refine(late.clone(), None), late);
        assert!(!heuristic_suffices(&refine(late.clone(), None)));
        // 8-byte shader hashes: 5.8, no prompt
        assert_eq!(refine(late.clone(), Some(8)), [EngineVersion::UE5_8]);
        assert!(heuristic_suffices(&refine(late.clone(), Some(8))));
        // 20-byte: 5.6 or 5.7
        assert_eq!(refine(late, Some(20)), [EngineVersion::UE5_6, EngineVersion::UE5_7]);
        // older engines are unaffected
        assert_eq!(refine(candidates(T::OnDemandMetaData, Some(H::NoExportInfo)), Some(20)), [EngineVersion::UE5_4]);
    }

    #[test]
    fn custom_version_markers() {
        let release = |v| {
            let mut b = [0u8; 16];
            for (i, w) in [0xD89B5E42u32, 0x24BD4D46, 0x8412ACA8, 0xDF641779].iter().enumerate() {
                b[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
            }
            (retoc::FGuid::from_bytes(b), v)
        };
        assert_eq!(saved_by_5_8(&[release(68)]), Some(true));
        assert_eq!(saved_by_5_8(&[release(61)]), Some(false));
        assert_eq!(saved_by_5_8(&[release(56)]), Some(false));
        assert_eq!(saved_by_5_8(&[]), None);
    }

    #[test]
    fn output_parts() {
        assert_eq!(Part::of_output("A/B.uasset"), Some(Part::Asset));
        assert_eq!(Part::of_output("A/B.umap"), Some(Part::Asset));
        assert_eq!(Part::of_output("A/B.m.ubulk"), Some(Part::MemoryMappedBulk));
        assert_eq!(Part::of_output("A/B.ubulk"), Some(Part::Bulk));
        assert_eq!(Part::of_output("A/B.uptnl"), Some(Part::OptionalBulk));
        assert_eq!(Part::of_output("A/B.uexp"), Some(Part::Exports));
        assert_eq!(Part::of_output("A/B.txt"), None);
    }
}
