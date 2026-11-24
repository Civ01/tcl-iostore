//! Packing raw Zen files (N1, SPEC §8).
//!
//! A packed file the game has is replaced in place in the target container (chunk ID, path and
//! store entry stay) or, found only in another container under the global's folder, added with
//! that container's chunk ID, path and (for packages) store entry. New packages and their bulk
//! files (N1b, SPEC §8.4) are planned in [`crate::add`]. Anything else cancels the whole copy.
//! New containers take their format from the game. The container edit itself is
//! [`iostore::edit::ContainerEdit`]: new data is appended, the `.utoc` replaced last, an undo file
//! kept while writing.

use crate::config::{MountPointMode, Options, PackCompression, PackOptions, ValidateOnAdd, ValidateOnReplace};
use crate::dlog;
use crate::view::Mount;
use iostore::edit::{ContainerEdit, NewContainer, UndoRecord, partition_path, strip_prefix_ignore_case};
use iostore::write::Compressor;
use iostore::{ChunkId, ChunkKind, Container, TocVersion};
use retoc::container_header::FIoContainerHeader;
use std::collections::HashMap;
use std::ffi::c_int;
use std::io::Cursor;
use std::path::{Path, PathBuf};

pub const PK_PACK_MOVE_FILES: c_int = 1;
pub const PK_PACK_SAVE_PATHS: c_int = 2;

/// Folders of the listing that can't be written.
const LEGACY_DIR: &str = crate::legacy::LEGACY_DIR;
const CHUNKS_DIR: &str = crate::archive::CHUNKS_DIR;

#[derive(Debug)]
pub enum PackError {
    /// Shown as a warning; nothing was changed.
    User(String),
    Aborted,
    /// Something failed while writing (the container was rolled back).
    Failed(String),
}

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackError::User(m) | PackError::Failed(m) => f.write_str(m),
            PackError::Aborted => f.write_str("cancelled"),
        }
    }
}

impl From<iostore::Error> for PackError {
    fn from(e: iostore::Error) -> Self {
        match e {
            iostore::Error::Cancelled => PackError::Aborted,
            other => PackError::Failed(other.to_string()),
        }
    }
}

impl From<std::io::Error> for PackError {
    fn from(e: std::io::Error) -> Self {
        PackError::Failed(e.to_string())
    }
}

pub fn undo_path(utoc: &Path) -> PathBuf {
    let mut name = utoc.file_name().unwrap_or_default().to_os_string();
    name.push(".uetoc-undo");
    utoc.with_file_name(name)
}

/// `.utoc` for a `.ucas`/`.utoc` name TC passes.
pub fn utoc_of(packed: &Path) -> PathBuf {
    match packed.extension().map(|e| e.to_ascii_lowercase()) {
        Some(e) if e == "utoc" => packed.to_path_buf(),
        _ => crate::read::utoc_for_ucas(packed).unwrap_or_else(|| packed.with_extension("utoc")),
    }
}

// ---------------------------------------------------------------------------
// The game: every container under the global's folder

/// A chunk somewhere in the game.
#[derive(Debug, Clone)]
pub struct GameFile {
    pub utoc: PathBuf,
    pub index: u32,
    pub id: ChunkId,
    /// Mount point + path, as UE builds it (`../../../Game/Content/a.uasset`).
    pub full: Option<String>,
    pub memory_mapped: bool,
}

#[derive(Debug, Clone)]
pub struct GameContainer {
    pub utoc: PathBuf,
    pub version: TocVersion,
    pub block_size: u32,
    pub methods: Vec<String>,
    pub is_global: bool,
}

pub struct Game {
    pub global: PathBuf,
    /// Lower-case game path (mount point's visible part + path, `/`) → files.
    pub by_path: HashMap<String, Vec<GameFile>>,
    pub by_id: HashMap<ChunkId, Vec<GameFile>>,
    pub containers: Vec<GameContainer>,
    /// Containers that couldn't be read (encrypted without a known key, damaged).
    pub unreadable: Vec<String>,
}

/// `Game/Content/a.uasset` for mount `../../../Game/Content/` + `a.uasset` (lower case).
pub fn game_key(mount: &str, rel: &str) -> String {
    let visible = Mount::parse(mount).visible.join("/");
    let k = if visible.is_empty() { rel.to_string() } else { format!("{visible}/{rel}") };
    k.to_lowercase()
}

pub(crate) fn open_quiet(utoc: &Path) -> Result<Container, String> {
    let mut c = Container::open(utoc, crate::profile::profile_for(utoc)).map_err(|e| e.to_string())?;
    if c.is_encrypted() {
        let folder = utoc.parent().unwrap_or(Path::new("")).to_path_buf();
        let guid = c.header().key_guid.to_string();
        if !crate::keys::candidates(&folder, &guid).into_iter().any(|(k, _, _)| c.set_key(k.to_iostore()).is_ok()) {
            return Err("encrypted, no known key".into());
        }
    }
    Ok(c)
}

impl Game {
    pub fn build(global: &Path) -> Game {
        let root = global.parent().unwrap_or(Path::new("."));
        let mut utocs = Vec::new();
        crate::legacy::find_utocs(root, &mut utocs, 0);
        let mut g = Game { global: global.to_path_buf(), by_path: HashMap::new(), by_id: HashMap::new(), containers: Vec::new(), unreadable: Vec::new() };
        for utoc in utocs {
            match open_quiet(&utoc) {
                Ok(c) => g.add_container(&utoc, c),
                Err(e) => {
                    dlog!("  game: skipped {}: {e}", utoc.display());
                    g.unreadable.push(format!("{} ({e})", utoc.display()));
                }
            }
        }
        g
    }

    /// Adds (or refreshes) one container.
    pub fn add_container(&mut self, utoc: &Path, mut c: Container) {
        let is_global = same_path(utoc, &self.global);
        self.by_path.values_mut().for_each(|v| v.retain(|f| !same_path(&f.utoc, utoc)));
        self.by_id.values_mut().for_each(|v| v.retain(|f| !same_path(&f.utoc, utoc)));
        self.containers.retain(|x| !same_path(&x.utoc, utoc));
        let index = c.directory_index().ok().cloned();
        let mut paths: Vec<Option<String>> = vec![None; c.chunk_count() as usize];
        if let Some(idx) = &index {
            for f in &idx.files {
                if let Some(p) = paths.get_mut(f.toc_index as usize) {
                    *p = Some(f.path.clone());
                }
            }
        }
        for info in c.chunks() {
            let rel = paths[info.index as usize].clone();
            let full = match (&index, &rel) {
                (Some(idx), Some(r)) => Some(format!("{}{r}", idx.mount_point)),
                _ => None,
            };
            let f = GameFile { utoc: utoc.to_path_buf(), index: info.index, id: info.id, full, memory_mapped: info.memory_mapped() };
            if let (Some(idx), Some(r)) = (&index, &rel) {
                self.by_path.entry(game_key(&idx.mount_point, r)).or_default().push(f.clone());
            }
            self.by_id.entry(info.id).or_default().push(f);
        }
        self.containers.push(GameContainer {
            utoc: utoc.to_path_buf(),
            version: c.version(),
            block_size: c.header().block_size,
            methods: c.toc().compression_methods.clone(),
            is_global,
        });
    }

    /// First match outside `except` (the target itself) and the global.
    fn find_elsewhere(&self, key: &str, except: &Path) -> Option<&GameFile> {
        self.by_path.get(key)?.iter().find(|f| !same_path(&f.utoc, except) && !same_path(&f.utoc, &self.global))
    }
    fn find_id_elsewhere(&self, id: &ChunkId, except: &Path) -> Option<&GameFile> {
        self.by_id.get(id)?.iter().find(|f| !same_path(&f.utoc, except) && !same_path(&f.utoc, &self.global))
    }
    fn in_global(&self, key: &str) -> bool {
        self.by_path.get(key).is_some_and(|v| v.iter().any(|f| same_path(&f.utoc, &self.global)))
    }
}

pub fn same_path(a: &Path, b: &Path) -> bool {
    let n = |p: &Path| p.to_string_lossy().replace('/', "\\").to_lowercase();
    n(a) == n(b)
}

// ---------------------------------------------------------------------------
// What TC hands over

/// One file to pack.
#[derive(Debug, Clone)]
pub struct Input {
    pub src: PathBuf,
    /// Path inside the archive as TC shows it (`\`-separated).
    pub archive_path: String,
    pub len: u64,
    /// A legacy package converted to Zen (N2): `src` is the converted file.
    pub converted: Option<crate::legacy_pack::Converted>,
}

/// Files from TC's list (directories, ending in `\`, are skipped). `SAVE_PATHS` off: file
/// names only.
pub fn inputs(src_path: &Path, sub_path: Option<&str>, items: &[String], flags: c_int) -> Result<Vec<Input>, PackError> {
    let mut out = Vec::new();
    for item in items {
        if item.ends_with('\\') || item.ends_with('/') {
            continue;
        }
        let src = src_path.join(item.replace(['\\', '/'], std::path::MAIN_SEPARATOR_STR));
        let meta = std::fs::metadata(&src).map_err(|e| PackError::User(format!("{}: {e}", src.display())))?;
        if meta.is_dir() {
            continue;
        }
        let name = item.replace('/', "\\");
        let rel = if flags & PK_PACK_SAVE_PATHS != 0 { name } else { name.rsplit('\\').next().unwrap_or(&name).to_string() };
        let archive_path = match sub_path.map(|s| s.trim_matches('\\')).filter(|s| !s.is_empty()) {
            Some(sub) => format!("{sub}\\{rel}"),
            None => rel,
        };
        out.push(Input { src, archive_path, len: meta.len(), converted: None });
    }
    if out.is_empty() {
        return Err(PackError::User("No files to pack (folders alone can't be stored).".into()));
    }
    Ok(out)
}

/// A planned change.
#[derive(Debug, Clone)]
pub enum Op {
    /// Chunk `index` of the target (as opened) gets new data.
    Replace { index: usize },
    /// Chunk from elsewhere in the game, added with its id and path (relative to the game root).
    Add { from: GameFile },
    /// A file the game doesn't have (N1b, SPEC §8.4): its full game path, its store entry
    /// (packages), memory-mapped (`.m.ubulk`).
    New { full: String, entry: Option<retoc::container_header::StoreEntry>, memory_mapped: bool },
}

impl Step {
    /// Full game path the step puts its file at (copies and new files).
    pub fn full(&self) -> Option<&str> {
        match &self.op {
            Op::Replace { .. } => None,
            Op::Add { from } => from.full.as_deref(),
            Op::New { full, .. } => Some(full),
        }
    }
}

/// The mount point `edit` needs so that every copied or new file fits (`None`: unchanged):
/// the deepest common folder of the current one and all of them (as tcl-uepak).
pub fn mount_change(edit: &ContainerEdit, steps: &[Step]) -> Option<String> {
    let m = edit.mount_point()?;
    let fulls: Vec<&str> = steps.iter().filter_map(Step::full).collect();
    if fulls.iter().all(|f| strip_prefix_ignore_case(f, m).is_some()) {
        return None;
    }
    let mut all: Vec<String> = edit.chunks().filter_map(|(_, _, p)| p.map(|p| format!("{m}{p}"))).collect();
    all.push(m.to_string());
    all.extend(fulls.iter().map(|s| s.to_string()));
    let root = iostore::common_root_path(all.iter().map(String::as_str));
    (!root.eq_ignore_ascii_case(m)).then_some(root)
}

#[derive(Debug, Clone)]
pub struct Step {
    pub input: Input,
    pub op: Op,
    pub id: ChunkId,
}

#[derive(Debug, Default)]
pub struct Refusals {
    pub not_in_game: Vec<String>,
    pub companion_pak: Vec<String>,
    pub legacy: Vec<String>,
    /// Legacy packages that couldn't be converted (N2): "path: reason".
    pub legacy_failed: Vec<String>,
    pub read_only: Vec<String>,
}

impl Refusals {
    pub fn is_empty(&self) -> bool {
        self.not_in_game.is_empty() && self.companion_pak.is_empty() && self.legacy.is_empty() && self.legacy_failed.is_empty() && self.read_only.is_empty()
    }
    pub fn message(&self, unreadable: &[String]) -> String {
        let mut s = String::from("Nothing was packed. These files can't go into this container:\n");
        let list = |s: &mut String, title: &str, v: &[String]| {
            if v.is_empty() {
                return;
            }
            s.push_str(&format!("\n{title}:\n"));
            for f in v.iter().take(15) {
                s.push_str(&format!("  {f}\n"));
            }
            if v.len() > 15 {
                s.push_str(&format!("  … and {} more\n", v.len() - 15));
            }
        };
        list(&mut s, "Not in the game, and not a package or bulk file (shader libraries and other chunk files can only replace the game's)", &self.not_in_game);
        list(&mut s, "Belong in the companion .pak, not in IoStore", &self.companion_pak);
        list(&mut s, "Legacy packages that can't be converted to Zen", &self.legacy_failed);
        list(&mut s, "A .uexp without its legacy .uasset/.umap (copy them together; they are converted to Zen)", &self.legacy);
        list(&mut s, "Read-only ($legacy view, the global container, the container header)", &self.read_only);
        if !unreadable.is_empty() && !self.not_in_game.is_empty() {
            s.push_str(&format!("\n{} container(s) of the game couldn't be searched:\n", unreadable.len()));
            for u in unreadable.iter().take(5) {
                s.push_str(&format!("  {u}\n"));
            }
        }
        s
    }
}

/// File types that only live in IoStore (everything else belongs in the companion `.pak`).
fn is_iostore_type(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    [".uasset", ".umap", ".ubulk", ".uptnl", ".ushaderbytecode"].iter().any(|e| p.ends_with(e))
}

/// Legacy cooked `.uasset`/`.umap`: starts with the package file tag.
pub(crate) fn is_legacy_package(path: &Path) -> bool {
    use std::io::Read;
    let mut b = [0u8; 4];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut b)).is_ok() && u32::from_le_bytes(b) == 0x9E2A_83C1
}

/// The archive path's game key, or `None` for `$chunks`/`$legacy` (handled separately).
fn archive_game_key(archive_path: &str, mode: MountPointMode, target_mount: Option<&str>) -> String {
    let rel = archive_path.replace('\\', "/");
    match (mode, target_mount) {
        (MountPointMode::Root, Some(m)) => game_key(m, &rel),
        _ => rel.to_lowercase(),
    }
}

/// Game key of a full path (`../../../A/B.uasset` -> `a/b.uasset`).
fn full_key(full: &str) -> String {
    crate::add::visible(full).to_lowercase()
}

/// Where a file copied into a new container at `rel` (`/`-separated) goes (SPEC §8.3):
/// - mount point `None` (Auto): `../../../` + rel, the container's mount point follows the files;
/// - otherwise mount point + rel, unless rel already starts with the mount point's folders
///   (`Moadra/Content/Mods/SML/…` for `../../../Moadra/Content/Mods/SML/`): then rel is the full
///   path. When both readings are possible, the one the game has wins (`in_game` takes a full
///   path), else the full-path reading.
///
/// Every result is under the mount point, so the mount point check can't fail.
pub fn new_container_full(rel: &str, mount: Option<&str>, in_game: impl Fn(&str) -> bool) -> String {
    let as_full = format!("../../../{rel}");
    let Some(m) = mount else { return as_full };
    let prefixed = format!("{m}{rel}");
    let tail = match strip_prefix_ignore_case(m, "../../../") {
        Some(t) if !t.is_empty() => t,
        _ => return prefixed,
    };
    if strip_prefix_ignore_case(rel, tail).is_none() {
        return prefixed;
    }
    if in_game(&prefixed) && !in_game(&as_full) { prefixed } else { as_full }
}

/// Classifies every input. `target` is the open edit (`None` for a new container, whose paths
/// come from `new_mount` via [`new_container_full`]).
pub fn plan(inputs: &[Input], game: &Game, utoc: &Path, target: Option<&ContainerEdit>, mode: MountPointMode, new_mount: Option<&str>) -> (Vec<Step>, Refusals, Vec<crate::add::Candidate>) {
    let mut steps = Vec::new();
    let mut r = Refusals::default();
    let mut new = Vec::new();
    let target_mount = target.and_then(|t| t.mount_point());
    // target's own files by game key and id
    let mut own: HashMap<String, (usize, ChunkId)> = HashMap::new();
    let mut own_ids: HashMap<ChunkId, usize> = HashMap::new();
    if let Some(t) = target {
        for (i, id, path) in t.chunks() {
            own_ids.insert(id, i);
            if let (Some(p), Some(m)) = (path, target_mount) {
                own.insert(game_key(m, p), (i, id));
            }
        }
    }
    let is_global = same_path(utoc, &game.global);
    for input in inputs {
        let shown = input.archive_path.clone();
        let first = shown.split('\\').next().unwrap_or("");
        if is_global || first.eq_ignore_ascii_case(LEGACY_DIR) {
            r.read_only.push(shown);
            continue;
        }
        if first.eq_ignore_ascii_case(CHUNKS_DIR) {
            let name = shown.rsplit('\\').next().unwrap_or("");
            let Some(id) = name.split('.').next().and_then(ChunkId::from_hex) else {
                r.not_in_game.push(shown);
                continue;
            };
            let version = target.map(|t| t.toc().header.version).or_else(|| game.containers.iter().find(|c| c.is_global).map(|c| c.version)).unwrap_or(TocVersion(8));
            if id.kind(version) == ChunkKind::ContainerHeader {
                r.read_only.push(shown);
            } else if let Some(&i) = own_ids.get(&id) {
                steps.push(Step { input: input.clone(), op: Op::Replace { index: i }, id });
            } else if let Some(f) = game.find_id_elsewhere(&id, utoc) {
                steps.push(Step { input: input.clone(), op: Op::Add { from: f.clone() }, id });
            } else {
                r.not_in_game.push(shown);
            }
            continue;
        }
        let new_full = target.is_none().then(|| new_container_full(&shown.replace('\\', "/"), new_mount, |f| game.by_path.contains_key(&full_key(f))));
        let key = match &new_full {
            Some(f) => full_key(f),
            None => archive_game_key(&shown, mode, target_mount),
        };
        let lower = key.to_ascii_lowercase();
        if (lower.ends_with(".uasset") || lower.ends_with(".umap") || lower.ends_with(".uexp")) && (lower.ends_with(".uexp") || is_legacy_package(&input.src)) {
            r.legacy.push(shown);
        } else if let Some(&(i, id)) = own.get(&key) {
            steps.push(Step { input: input.clone(), op: Op::Replace { index: i }, id });
        } else if let Some(f) = game.find_elsewhere(&key, utoc) {
            steps.push(Step { input: input.clone(), op: Op::Add { from: f.clone() }, id: f.id });
        } else if game.in_global(&key) {
            r.read_only.push(shown);
        } else if matches!(crate::add::ext_of(&key).as_str(), "uasset" | "umap" | "ubulk" | "uptnl" | "m.ubulk") {
            // not in the game: a new file (N1b, SPEC §8.4)
            let rel = shown.replace('\\', "/");
            let full = match (mode, target_mount, new_full) {
                (_, _, Some(f)) => f,
                (MountPointMode::Root, Some(m), None) => format!("{m}{rel}"),
                (_, Some(m), None) => format!("{}{rel}", crate::view::Mount::parse(m).base),
                (_, None, None) => format!("../../../{rel}"),
            };
            new.push(crate::add::Candidate { input: input.clone(), full });
        } else if is_iostore_type(&key) {
            r.not_in_game.push(shown);
        } else {
            r.companion_pak.push(shown);
        }
    }
    (steps, r, new)
}

// ---------------------------------------------------------------------------
// Compression

fn compressor_for(name: &str, opts: &PackOptions) -> Option<Compressor> {
    match name.to_ascii_lowercase().as_str() {
        "zlib" => Some(Compressor::Zlib(opts.zlib_level)),
        "gzip" => Some(Compressor::Gzip(opts.zlib_level)),
        "lz4" => Some(Compressor::Lz4),
        "oodle" => Some(Compressor::Oodle(opts.oodle_compressor, opts.oodle_level)),
        _ => None,
    }
}

/// `Compression=Match`: the target's own method; for a new container the method most of the
/// game's containers use (none when most are uncompressed).
pub fn choose_compressor(opts: &PackOptions, target_methods: Option<&[String]>, game: &Game) -> Result<Option<Compressor>, PackError> {
    let c = match opts.compression {
        PackCompression::None => None,
        PackCompression::Zlib => compressor_for("zlib", opts),
        PackCompression::Gzip => compressor_for("gzip", opts),
        PackCompression::Lz4 => compressor_for("lz4", opts),
        PackCompression::Oodle => compressor_for("oodle", opts),
        PackCompression::Match => match target_methods {
            Some(m) => m.first().and_then(|n| compressor_for(n, opts)),
            None => {
                let mut count: HashMap<String, usize> = HashMap::new();
                for c in game.containers.iter().filter(|c| !c.is_global) {
                    *count.entry(c.methods.first().map(|m| m.to_ascii_lowercase()).unwrap_or_default()).or_default() += 1;
                }
                let best = count.into_iter().max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0))).map(|x| x.0).unwrap_or_default();
                compressor_for(&best, opts)
            }
        },
    };
    if let (Some(Compressor::Oodle(..)), Err(e)) = (&c, iostore::oodle_status()) {
        let deps = crate::config::deps_dir().map(|d| d.display().to_string()).unwrap_or_default();
        return Err(PackError::User(format!("Oodle compression needs the Oodle library (oo2core_9_win64.dll) in\n{deps}\n\n{e}\n\nOr set [Pack] Compression=None in uetoc.ini.")));
    }
    Ok(c)
}

// ---------------------------------------------------------------------------
// Container header

fn read_header(c: &Container) -> Result<Option<(u32, FIoContainerHeader)>, String> {
    let Some(info) = c.chunks().find(|i| i.kind == ChunkKind::ContainerHeader) else { return Ok(None) };
    let data = c.read_chunk(info.index).map_err(|e| e.to_string())?;
    let h = FIoContainerHeader::deserialize(&mut Cursor::new(&data), None).map_err(|e| format!("container header: {e:#}"))?;
    Ok(Some((info.index, h)))
}

fn serialize_header(h: &FIoContainerHeader) -> Result<Vec<u8>, String> {
    let mut c = Cursor::new(Vec::new());
    h.serialize(&mut c).map_err(|e| format!("container header: {e:#}"))?;
    let mut data = c.into_inner();
    // UE (and retoc) keep the container header chunk aligned to 16.
    data.resize(data.len().next_multiple_of(16), 0);
    Ok(data)
}

/// Store entries of packages taken from other containers, cached per container.
#[derive(Default)]
pub(crate) struct HeaderCache(HashMap<String, Option<FIoContainerHeader>>);

impl HeaderCache {
    pub(crate) fn get(&mut self, utoc: &Path) -> Result<Option<&FIoContainerHeader>, String> {
        let k = utoc.to_string_lossy().to_lowercase();
        if !self.0.contains_key(&k) {
            let c = open_quiet(utoc)?;
            self.0.insert(k.clone(), read_header(&c)?.map(|x| x.1));
        }
        Ok(self.0.get(&k).and_then(|h| h.as_ref()))
    }
}

fn is_package(id: &ChunkId, version: TocVersion) -> bool {
    id.kind(version) == ChunkKind::ExportBundleData
}

// ---------------------------------------------------------------------------
// Running an edit

#[derive(Debug)]
pub struct Stats {
    pub replaced: usize,
    pub added: usize,
    pub removed: usize,
    pub input_bytes: u64,
    pub compacted: bool,
    pub new_container: bool,
    pub pak_written: Option<PathBuf>,
}

/// Progress for one file: reports encoded bytes, `true` = cancel.
fn progress_for(input: &Path) -> impl FnMut(u64) -> bool {
    let mut name = crate::win::wide(&input.to_string_lossy());
    let mut last = 0u64;
    move |done: u64| {
        let delta = done - last;
        last = done;
        !crate::progress::report(&mut name, delta)
    }
}

/// Free space on the target's volume against `need` bytes (unknown = fine).
fn check_space(dir: &Path, need: u64) -> Result<(), PackError> {
    if let Some(free) = crate::win::free_space(dir).filter(|&free| free < need + (16 << 20)) {
        return Err(PackError::User(format!("Not enough free space in {}: about {} MB needed, {} MB free.", dir.display(), (need >> 20) + 16, free >> 20)));
    }
    Ok(())
}

/// The target's container header: chunk index (as opened) and parsed header.
type TargetHeader = Option<(Option<usize>, FIoContainerHeader)>;

/// Opens the target for editing (key resolved with the read lookup, prompting if needed), with
/// its container header.
fn open_edit(utoc: &Path) -> Result<(ContainerEdit, TargetHeader), PackError> {
    let profile = crate::profile::profile_for(utoc);
    let mut c = Container::open(utoc, profile.clone()).map_err(|e| PackError::User(format!("{}: {e}", utoc.display())))?;
    if c.is_encrypted() {
        match crate::read::resolve_key(&mut c, utoc) {
            Ok(()) => {}
            Err(crate::read::KeyOutcome::Cancelled) => return Err(PackError::Aborted),
            Err(crate::read::KeyOutcome::Failed(e)) => return Err(PackError::User(format!("{}: {e}", utoc.display()))),
        }
    }
    let key = c.key().cloned();
    let header = read_header(&c).map_err(|e| PackError::User(format!("{}: {e}", utoc.display())))?.map(|(i, h)| (Some(i as usize), h));
    drop(c);
    let edit = ContainerEdit::open(utoc, profile.as_ref(), key).map_err(|e| PackError::User(format!("{}: {e}", utoc.display())))?;
    Ok((edit, header))
}

/// Applies `steps` (and `removals`, chunk indices as opened) to `edit`, updating the container
/// header. Any error rolls the partitions back; the `.utoc` is only written on success.
fn apply(
    mut edit: ContainerEdit,
    utoc: &Path,
    steps: &[Step],
    removals: &[usize],
    compressor: Option<&Compressor>,
    mut header: TargetHeader,
    fixes: &HashMap<ChunkId, retoc::container_header::StoreEntry>,
) -> Result<(usize, usize, usize), PackError> {
    let version = edit.toc().header.version;
    let undo = undo_path(utoc);
    std::fs::write(&undo, edit.undo_record()?.to_bytes())?;
    let result = (|| -> Result<(usize, usize, usize), PackError> {
        let mut header_changed = false;
        let mut cache = HeaderCache::default();
        let (mut replaced, mut added) = (0, 0);
        let target_mount = edit.mount_point().map(String::from);
        // mount point that covers every copied or new file (asked about before)
        if let (Some(m), Some(root)) = (&target_mount, mount_change(&edit, steps)) {
            dlog!("  mount point {m:?} -> {root:?} (files outside it)");
            edit.set_mount_point(&root)?;
        }
        let mount = edit.mount_point().map(String::from);
        for s in steps {
            let mut f = std::fs::File::open(&s.input.src).map_err(|e| PackError::Failed(format!("{}: {e}", s.input.src.display())))?;
            let mut cancel = progress_for(&s.input.src);
            match &s.op {
                Op::Replace { index } => {
                    edit.replace(*index, &mut f, s.input.len, compressor, &mut cancel)?;
                    replaced += 1;
                    if let Some((_, h)) = &mut header {
                        let package = retoc::FPackageId(s.id.id64());
                        // a converted legacy package brings its own entry (N2)
                        let entry = fixes.get(&s.id).cloned().or_else(|| s.input.converted.as_ref().map(|c| crate::legacy_pack::merged_entry(&c.entry, h.get_store_entry(package).as_ref())));
                        if let Some(entry) = entry {
                            h.add_package(package, entry);
                            header_changed = true;
                        }
                    }
                }
                Op::New { full, entry, memory_mapped } => {
                    let rel = match &mount {
                        Some(m) => Some(strip_prefix_ignore_case(full, m).ok_or_else(|| PackError::Failed(format!("{full} is outside {m}")))?.to_string()),
                        None => None,
                    };
                    edit.add(s.id, rel, &mut f, s.input.len, compressor, *memory_mapped, &mut cancel)?;
                    added += 1;
                    if let Some(entry) = entry {
                        match &mut header {
                            Some((_, h)) => {
                                h.add_package_with_soft_references(retoc::FPackageId(s.id.id64()), entry.clone(), Vec::new()).map_err(|e| PackError::Failed(format!("{e:#}")))?;
                                header_changed = true;
                            }
                            None => return Err(PackError::Failed("the target container has no container header for the new package".into())),
                        }
                    }
                }
                Op::Add { from } => {
                    let rel = match (&from.full, &mount) {
                        (Some(full), Some(m)) => Some(strip_prefix_ignore_case(full, m).ok_or_else(|| PackError::Failed(format!("{full} is outside {m}")))?.to_string()),
                        _ => None,
                    };
                    edit.add(s.id, rel, &mut f, s.input.len, compressor, from.memory_mapped, &mut cancel)?;
                    added += 1;
                    if is_package(&s.id, version) {
                        let src = cache.get(&from.utoc).map_err(PackError::Failed)?;
                        let package = retoc::FPackageId(s.id.id64());
                        let game_entry = src.and_then(|h| h.get_store_entry(package));
                        let entry = fixes.get(&s.id).cloned().or_else(|| s.input.converted.as_ref().map(|c| crate::legacy_pack::merged_entry(&c.entry, game_entry.as_ref()))).or(game_entry);
                        let refs = src.map(|h| h.soft_references().map(|r| r.get(&package).cloned().unwrap_or_default())).transpose().map_err(|e| PackError::Failed(format!("{e:#}")))?.unwrap_or_default();
                        match (&mut header, entry) {
                            (Some((_, h)), Some(entry)) => {
                                h.add_package_with_soft_references(package, entry, refs).map_err(|e| PackError::Failed(format!("{e:#}")))?;
                                header_changed = true;
                            }
                            (None, _) => return Err(PackError::Failed("the target container has no container header for the added package".into())),
                            (_, None) => return Err(PackError::Failed(format!("{}: no store entry for {} in {}", s.input.archive_path, s.id.hex(), from.utoc.display()))),
                        }
                    }
                }
            }
        }
        let mut removed = 0;
        for &i in removals {
            let id = edit.chunks().find(|(x, _, _)| *x == i).map(|(_, id, _)| id);
            edit.remove(i)?;
            removed += 1;
            if let (Some(id), Some((_, h))) = (id, &mut header) {
                if is_package(&id, version) && h.remove_package(retoc::FPackageId(id.id64())).map_err(|e| PackError::Failed(format!("{e:#}")))?.is_some() {
                    header_changed = true;
                }
            }
        }
        if let Some((index, h)) = header.as_ref().filter(|_| header_changed) {
            let data = serialize_header(h).map_err(PackError::Failed)?;
            match index {
                Some(i) => edit.replace(*i, &mut data.as_slice(), data.len() as u64, None, &mut |_| false)?,
                None => {
                    let id = ChunkId::create(h.container_id.0, 0, ChunkKind::ContainerHeader, version).ok_or_else(|| PackError::Failed("no container header chunk type".into()))?;
                    edit.add(id, None, &mut data.as_slice(), data.len() as u64, None, false, &mut |_| false)?;
                }
            }
        }
        Ok((replaced, added, removed))
    })();
    match result {
        Ok(counts) => {
            edit.finish()?;
            let _ = std::fs::remove_file(&undo);
            Ok(counts)
        }
        Err(e) => {
            dlog!("  rolling back: {e}");
            if let Err(r) = edit.rollback() {
                dlog!("  rollback failed: {r}");
            }
            let _ = std::fs::remove_file(&undo);
            Err(e)
        }
    }
}

// ---------------------------------------------------------------------------
// ValidateOnReplace (SPEC §8.5)

/// Result of checking the packages among `steps`.
#[derive(Default)]
pub struct Validation {
    /// Corrected store entries by package chunk (Fix mode only).
    pub fixes: HashMap<ChunkId, retoc::container_header::StoreEntry>,
    pub findings: Vec<crate::validate::Finding>,
}

impl Validation {
    pub fn any_fixable(&self) -> bool {
        self.findings.iter().any(|f| f.issues.iter().any(|i| i.fixable()))
    }
}

fn toc_enum(v: TocVersion) -> Result<retoc::EIoStoreTocVersion, String> {
    retoc::EIoStoreTocVersion::from_repr(v.0).ok_or_else(|| format!("TOC version {}", v.0))
}

/// Game packages whose store entry imports `package` (display paths), across all containers.
fn importers(game: &Game, package: retoc::FPackageId, cache: &mut HeaderCache) -> Vec<String> {
    let mut out = Vec::new();
    for c in game.containers.iter().filter(|c| !c.is_global) {
        let Ok(Some(h)) = cache.get(&c.utoc) else { continue };
        for id in h.package_ids() {
            if h.get_store_entry(id).is_some_and(|e| e.imported_packages.contains(&package)) {
                let label = ChunkId::create(id.0, 0, ChunkKind::ExportBundleData, c.version)
                    .and_then(|chunk| game.by_id.get(&chunk))
                    .and_then(|v| v.iter().find_map(|f| f.full.clone()))
                    .map(|f| f.trim_start_matches("../").to_string())
                    .unwrap_or_else(|| format!("{:016x}", id.0));
                out.push(label);
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Checks every package in `steps` against the game's version and the store entry packing
/// reuses (the target's for a replace, the source container's for a copy). Fixes are only
/// collected when `fix`.
pub fn validate_steps(steps: &[Step], game: &Game, utoc: &Path, target_header: Option<&FIoContainerHeader>, fix: bool) -> Validation {
    use crate::validate::{self, Finding, Issue};
    let mut v = Validation::default();
    let mut cache = HeaderCache::default();
    let mut opened: HashMap<String, Container> = HashMap::new();
    for s in steps {
        let (src_utoc, index) = match &s.op {
            Op::Replace { index } => (utoc.to_path_buf(), *index as u32),
            Op::Add { from } => (from.utoc.clone(), from.index),
            Op::New { .. } => continue,
        };
        let Some(gc) = game.containers.iter().find(|c| same_path(&c.utoc, &src_utoc)) else { continue };
        if !is_package(&s.id, gc.version) {
            continue;
        }
        let package = retoc::FPackageId(s.id.id64());
        let file = s.input.archive_path.clone();
        let one = |issue: Issue| Finding { file: file.clone(), issues: vec![issue], importers: vec![], importer_count: 0 };
        let header = match &s.op {
            Op::Replace { .. } => target_header,
            Op::Add { from } => cache.get(&from.utoc).ok().flatten(),
            Op::New { .. } => continue,
        };
        let Some(header) = header else { continue };
        let (hv, Some(entry)) = (header.version, header.get_store_entry(package)) else { continue };
        let toc = match toc_enum(gc.version) {
            Ok(t) => t,
            Err(e) => {
                v.findings.push(one(Issue::Unreadable(e)));
                continue;
            }
        };
        let key = src_utoc.to_string_lossy().to_lowercase();
        if !opened.contains_key(&key) {
            match open_quiet(&src_utoc) {
                Ok(c) => {
                    opened.insert(key.clone(), c);
                }
                Err(e) => {
                    v.findings.push(one(Issue::Unreadable(format!("the game's version: {e}"))));
                    continue;
                }
            }
        }
        let old = opened[&key].read_chunk(index).map_err(|e| e.to_string()).and_then(|d| validate::read_header(&mut Cursor::new(d), Some(entry.clone()), toc, hv));
        let old = match old {
            Ok(h) => h,
            Err(e) => {
                v.findings.push(one(Issue::Unreadable(format!("the game's version: {e}"))));
                continue;
            }
        };
        // a converted legacy package is read and checked with the entry to-zen built for it (N2)
        let entry = match &s.input.converted {
            Some(c) => crate::legacy_pack::merged_entry(&c.entry, Some(&entry)),
            None => entry,
        };
        let new_entry = if validate::names_imports(hv) { None } else { Some(entry.clone()) };
        let new = std::fs::File::open(&s.input.src).map_err(|e| e.to_string()).and_then(|f| validate::read_header(&mut std::io::BufReader::new(f), new_entry, toc, hv));
        let new = match new {
            Ok(h) => h,
            // before 5.3 a package is read with its store entry: a misfit is itself the finding
            Err(e) if !validate::names_imports(hv) => {
                v.findings.push(one(Issue::Unreadable(format!("doesn't fit the game's store entry for this package (export bundles or imports differ): {e}"))));
                continue;
            }
            Err(e) => {
                v.findings.push(one(Issue::Unreadable(e)));
                continue;
            }
        };
        let check = validate::compare(&old, &new, &entry, hv);
        if check.issues.is_empty() {
            continue;
        }
        let mut finding = Finding { file, issues: check.issues, importers: vec![], importer_count: 0 };
        if finding.issues.iter().any(|i| i.affects_importers()) {
            let all = importers(game, package, &mut cache);
            finding.importer_count = all.len();
            finding.importers = all.into_iter().take(10).collect();
        }
        for i in &finding.issues {
            dlog!("  validate {}: {}", finding.file, i.describe());
        }
        if fix {
            if let Some(e) = check.fixed_entry {
                v.fixes.insert(s.id, e);
            }
        }
        v.findings.push(finding);
    }
    v
}

/// What `ValidateOnReplace` decides before anything is written. `Ok` = go on.
fn decide_validation(mode: ValidateOnReplace, v: &Validation) -> Result<(), PackError> {
    use crate::validate::report;
    if v.findings.is_empty() {
        return Ok(());
    }
    match mode {
        ValidateOnReplace::Off | ValidateOnReplace::Fix => Ok(()),
        ValidateOnReplace::Error => Err(PackError::User(format!(
            "Nothing was packed. These files differ from the game's versions in ways that can break loading:\n{}\n(ValidateOnReplace=Error. Fix corrects the imports where possible; Warn asks; Off packs without checks.)",
            report(&v.findings, false)
        ))),
        ValidateOnReplace::Warn => {
            let hint = if v.any_fixable() { "\nValidateOnReplace=Fix would correct the issues marked [fixable].\n" } else { "" };
            let text = format!("These files differ from the game's versions in ways that can break loading:\n{}{hint}\nPack them anyway?", report(&v.findings, false));
            if crate::dialog::confirm(&text) {
                Ok(())
            } else {
                Err(PackError::Aborted)
            }
        }
    }
}

/// After a successful Fix-mode pack: what was fixed and what couldn't be.
fn report_fixes(v: &Validation) {
    if v.findings.is_empty() {
        return;
    }
    let fixed = v.findings.iter().flat_map(|f| &f.issues).filter(|i| i.fixable()).count();
    let open = v.findings.iter().flat_map(|f| &f.issues).filter(|i| !i.fixable()).count();
    let text = format!(
        "Packed. ValidateOnReplace=Fix: {fixed} fixed, {open} can't be fixed (packed as they are):\n{}",
        crate::validate::report(&v.findings, true)
    );
    if open > 0 { crate::win::warn(&text) } else { crate::win::info(&text) }
}

// ---------------------------------------------------------------------------
// N1b: new files (SPEC §8.4)

/// The container-header version of the game (any non-global container's header).
fn game_header_version(game: &Game) -> Option<retoc::container_header::EIoContainerHeaderVersion> {
    for c in game.containers.iter().filter(|c| !c.is_global) {
        let Ok(container) = open_quiet(&c.utoc) else { continue };
        if let Ok(Some((_, h))) = read_header(&container) {
            return Some(h.version);
        }
    }
    None
}

/// Plans the new files of a copy (`header`: the target's container-header version, if known).
fn plan_new_files(cands: &[crate::add::Candidate], game: &Game, toc: TocVersion, header: Option<retoc::container_header::EIoContainerHeaderVersion>, mode: ValidateOnAdd) -> crate::add::NewPlan {
    if cands.is_empty() {
        return crate::add::NewPlan::default();
    }
    let header = header.or_else(|| game_header_version(game));
    let package_in_game = |id: u64| -> Option<String> {
        let chunk = ChunkId::create(id, 0, ChunkKind::ExportBundleData, toc)?;
        let files = game.by_id.get(&chunk)?;
        Some(files.iter().find_map(|f| f.full.as_deref().map(|p| crate::add::visible(p).to_string())).unwrap_or_else(|| format!("{} in {}", chunk.hex(), files[0].utoc.display())))
    };
    let id_at_path = |key: &str| -> Option<u64> { game.by_path.get(key)?.iter().find(|f| is_package(&f.id, toc)).map(|f| f.id.id64()) };
    let visibles: Vec<String> = game.by_path.values().flatten().filter_map(|f| f.full.as_deref().map(|p| crate::add::visible(p).to_string())).collect();
    let view = crate::add::GameView { toc, header, package_in_game: &package_in_game, id_at_path: &id_at_path, visibles: &visibles };
    let plan = crate::add::plan_new(cands, &view, mode);
    for f in &plan.findings {
        dlog!("  add {}: {}", f.file, f.issue.describe());
    }
    for (f, r) in &plan.refused {
        dlog!("  add {f}: refused: {r}");
    }
    plan
}

/// Refusals and `ValidateOnAdd=Error` for the new files, before anything is asked or written.
fn check_new_files(plan: &crate::add::NewPlan, mode: ValidateOnAdd) -> Result<(), PackError> {
    if !plan.refused.is_empty() {
        return Err(PackError::User(crate::add::refused_text(&plan.refused)));
    }
    if mode == ValidateOnAdd::Error && !plan.findings.is_empty() {
        return Err(PackError::User(format!(
            "Nothing was packed. These new files don't fit the game (ValidateOnAdd=Error):\n\n{}\n(Fix moves files to the path their name gives; Warn lists the findings and asks; Off packs without checks.)",
            crate::add::findings_text(&plan.findings, false)
        )));
    }
    Ok(())
}

/// The one question for a copy with new files and/or a mount-point change (`mount`: the old and
/// new mount point and the first existing entry). `Ok` = go on.
fn ask_additions(utoc: &Path, steps: &[Step], plan: &crate::add::NewPlan, mode: ValidateOnAdd, existing: bool, mount: Option<(&str, &str, Option<&str>)>) -> Result<(), PackError> {
    let new_files: Vec<String> = steps.iter().filter_map(|s| if let Op::New { full, .. } = &s.op { Some(crate::add::visible(full).to_string()) } else { None }).collect();
    if new_files.is_empty() && mount.is_none() {
        return Ok(());
    }
    let findings = match mode {
        ValidateOnAdd::Off => String::new(),
        ValidateOnAdd::Fix => crate::add::findings_text(&plan.findings.iter().filter(|f| !f.issue.fixable()).cloned().collect::<Vec<_>>(), true),
        _ => crate::add::findings_text(&plan.findings, false),
    };
    let mount_text = mount.map(|(old, new, example)| {
        let first_new = steps.iter().filter_map(Step::full).find(|f| strip_prefix_ignore_case(f, old).is_none());
        crate::add::mount_text(&crate::add::MountChange { old, new, example_existing: example, example_new: first_new })
    });
    let text = crate::add::question(utoc, &new_files, existing, &findings, &plan.moved, mount_text.as_deref());
    if crate::dialog::confirm(&text) {
        Ok(())
    } else {
        Err(PackError::Aborted)
    }
}

/// Total size of the container's files.
fn container_bytes(utoc: &Path) -> u64 {
    let mut n = std::fs::metadata(utoc).map(|m| m.len()).unwrap_or(0);
    for i in 0.. {
        match std::fs::metadata(partition_path(utoc, i)) {
            Ok(m) => n += m.len(),
            Err(_) => break,
        }
    }
    n
}

/// `PackFiles` into an existing or new container.
pub fn pack_files(packed: &Path, sub_path: Option<&str>, src_path: &Path, items: &[String], flags: c_int, options: &Options) -> Result<Stats, PackError> {
    let opts = &options.pack;
    let utoc = utoc_of(packed);
    let inputs = inputs(src_path, sub_path, items, flags)?;
    let exists = utoc.is_file();
    let global = crate::global::find_global(&utoc, options.global_lookup_limit)
        .map_err(|nf| PackError::User(crate::global::not_found_text(&utoc, &nf)))?
        .utoc;
    if same_path(&utoc, &global) {
        return Err(PackError::User("The global container can't be changed.".into()));
    }
    let Some(_guard) = crate::busy::acquire(&utoc) else {
        return Err(PackError::User(format!("{}\n\nThe container is being changed by another operation. Try again when it has finished.", utoc.display())));
    };
    if undo_path(&utoc).exists() {
        return Err(PackError::User(format!("{}\n\nAn interrupted earlier change is pending. Open the container to restore or discard it first.", utoc.display())));
    }
    let started = std::time::Instant::now();
    let game = Game::build(&global);
    dlog!("  game: {} containers, {} paths, {} unreadable, {} ms", game.containers.len(), game.by_path.len(), game.unreadable.len(), started.elapsed().as_millis());
    if exists {
        let (edit, header) = open_edit(&utoc)?;
        // legacy packages → Zen (N2)
        let target_mount = edit.mount_point().map(String::from);
        let full_of = |p: &str| -> String {
            let rel = p.replace('\\', "/");
            match (options.mount_point, &target_mount) {
                (MountPointMode::Root, Some(m)) => format!("{m}{rel}"),
                (_, Some(m)) => format!("{}{rel}", crate::view::Mount::parse(m).base),
                (_, None) => format!("../../../{rel}"),
            }
        };
        let hv = header.as_ref().map(|h| h.1.version).or_else(|| game_header_version(&game));
        let prepared = crate::legacy_pack::prepare(inputs, &game, edit.toc().header.version, hv, &full_of)?;
        if prepared.converted > 0 {
            dlog!("  {} legacy packages converted to Zen", prepared.converted);
        }
        let inputs = &prepared.inputs;
        let input_bytes: u64 = inputs.iter().map(|i| i.len).sum();
        let (mut steps, mut refusals, cands) = plan(inputs, &game, &utoc, Some(&edit), options.mount_point, None);
        refusals.legacy_failed = prepared.refused.iter().map(|(f, r)| format!("{f}: {r}")).collect();
        if !refusals.is_empty() {
            return Err(PackError::User(refusals.message(&game.unreadable)));
        }
        let new_plan = plan_new_files(&cands, &game, edit.toc().header.version, header.as_ref().map(|h| h.1.version), opts.validate_on_add);
        check_new_files(&new_plan, opts.validate_on_add)?;
        steps.extend(new_plan.steps.iter().cloned());
        if edit.is_signed() && !crate::dialog::confirm(&format!("{}\n\nThis container is signed. After the change it is written unsigned; a game that checks signatures may refuse it.\n\nContinue?", utoc.display())) {
            return Err(PackError::Aborted);
        }
        let validation = if opts.validate_on_replace == ValidateOnReplace::Off {
            Validation::default()
        } else {
            validate_steps(&steps, &game, &utoc, header.as_ref().map(|h| &h.1), opts.validate_on_replace == ValidateOnReplace::Fix)
        };
        decide_validation(opts.validate_on_replace, &validation)?;
        let new_mount = mount_change(&edit, &steps);
        let first_entry = edit.chunks().find_map(|(_, _, p)| p.map(String::from));
        let old_mount = edit.mount_point().unwrap_or("").to_string();
        ask_additions(&utoc, &steps, &new_plan, opts.validate_on_add, true, new_mount.as_deref().map(|n| (old_mount.as_str(), n, first_entry.as_deref())))?;
        let methods = edit.toc().compression_methods.clone();
        let compressor = choose_compressor(opts, Some(&methods), &game)?;
        dlog!(
            "  {} steps ({} replace, {} copy, {} new), compression {compressor:?}",
            steps.len(),
            steps.iter().filter(|s| matches!(s.op, Op::Replace { .. })).count(),
            steps.iter().filter(|s| matches!(s.op, Op::Add { .. })).count(),
            steps.iter().filter(|s| matches!(s.op, Op::New { .. })).count()
        );
        check_space(utoc.parent().unwrap_or(Path::new(".")), input_bytes)?;
        let (replaced, added, removed) = apply(edit, &utoc, &steps, &[], compressor.as_ref(), header, &validation.fixes)?;
        if opts.validate_on_replace == ValidateOnReplace::Fix {
            report_fixes(&validation);
        }
        let compacted = maybe_compact(&utoc, opts)?;
        Ok(Stats { replaced, added, removed, input_bytes, compacted, new_container: false, pak_written: None })
    } else {
        let global_toc = game.containers.iter().find(|c| c.is_global).map(|c| c.version).unwrap_or(TocVersion(8));
        let mount = choose_new_mount(&utoc, &inputs, &game, global_toc, opts)?;
        // legacy packages → Zen (N2), at the paths the mount point gives
        let in_game = |f: &str| game.by_path.contains_key(&full_key(f));
        let full_of = |p: &str| -> String { new_container_full(&p.replace('\\', "/"), mount.as_deref(), in_game) };
        let prepared = crate::legacy_pack::prepare(inputs, &game, global_toc, game_header_version(&game), &full_of)?;
        if prepared.converted > 0 {
            dlog!("  {} legacy packages converted to Zen", prepared.converted);
        }
        let inputs = &prepared.inputs;
        let input_bytes: u64 = inputs.iter().map(|i| i.len).sum();
        dlog!("  new container mount point: {}", mount.as_deref().unwrap_or("Auto"));
        let hint = |e: PackError| match e {
            PackError::User(m) => PackError::User(format!("{m}\n\nMount point for the copied files: {}", mount.as_deref().unwrap_or("Auto (the common folder of the files)"))),
            e => e,
        };
        let (mut steps, mut refusals, cands) = plan(inputs, &game, &utoc, None, options.mount_point, mount.as_deref());
        refusals.legacy_failed = prepared.refused.iter().map(|(f, r)| format!("{f}: {r}")).collect();
        if !refusals.is_empty() {
            return Err(hint(PackError::User(refusals.message(&game.unreadable))));
        }
        let new_plan = plan_new_files(&cands, &game, global_toc, None, opts.validate_on_add);
        check_new_files(&new_plan, opts.validate_on_add).map_err(hint)?;
        steps.extend(new_plan.steps.iter().cloned());
        let validation = if opts.validate_on_replace == ValidateOnReplace::Off {
            Validation::default()
        } else {
            validate_steps(&steps, &game, &utoc, None, opts.validate_on_replace == ValidateOnReplace::Fix)
        };
        decide_validation(opts.validate_on_replace, &validation)?;
        ask_additions(&utoc, &steps, &new_plan, opts.validate_on_add, false, None)?;
        let settings = new_container_settings(&utoc, &game, &steps, mount.as_deref())?;
        let compressor = choose_compressor(opts, None, &game)?;
        let header = new_container_header(&game, &settings)?;
        dlog!("  new container: TOC {} block {} mount {:?} id {} compression {compressor:?}", settings.version.0, settings.block_size, settings.mount_point, settings.container_id);
        check_space(utoc.parent().unwrap_or(Path::new(".")), input_bytes)?;
        let edit = ContainerEdit::create(&utoc, &settings)?;
        // a new container has a header from the start (written on the first package)
        let header = header.map(|h| (None, h));
        let (_, added, _) = apply(edit, &utoc, &steps, &[], compressor.as_ref(), header, &validation.fixes).inspect_err(|_| {
            let _ = std::fs::remove_file(&utoc);
            let _ = std::fs::remove_file(partition_path(&utoc, 0));
        })?;
        let pak = utoc.with_extension("pak");
        let mut pak_written = None;
        if !pak.exists() {
            if opts.write_empty_pak {
                std::fs::write(&pak, empty_pak())?;
                pak_written = Some(pak);
            } else if opts.warn_pak_needed {
                crate::win::warn(&format!(
                    "{}\n\nThe container was created. The game mounts IoStore containers through a .pak of the same name; without one it won't load this container.\n\n(Set [Pack] WriteEmptyPak=1 to write an empty one, or WarnPakNeeded=0 to stop this message.)",
                    utoc.display()
                ));
            }
        }
        Ok(Stats { replaced: 0, added, removed: 0, input_bytes, compacted: false, new_container: true, pak_written })
    }
}

/// `DeleteFiles`: TC paths (folders as `dir\*.*`).
pub fn delete_files(packed: &Path, items: &[String], options: &Options) -> Result<Stats, PackError> {
    let utoc = utoc_of(packed);
    let global = crate::global::find_global(&utoc, options.global_lookup_limit).map(|g| g.utoc).ok();
    if global.as_deref().is_some_and(|g| same_path(g, &utoc)) {
        return Err(PackError::User("The global container can't be changed.".into()));
    }
    let Some(_guard) = crate::busy::acquire(&utoc) else {
        return Err(PackError::User(format!("{}\n\nThe container is being changed by another operation.", utoc.display())));
    };
    if undo_path(&utoc).exists() {
        return Err(PackError::User(format!("{}\n\nAn interrupted earlier change is pending. Open the container to restore or discard it first.", utoc.display())));
    }
    let (edit, header) = open_edit(&utoc)?;
    let version = edit.toc().header.version;
    let mount = edit.mount_point().map(String::from);
    let prefix = match (options.mount_point, &mount) {
        (MountPointMode::Folder, Some(m)) => Mount::parse(m).display_prefix(),
        _ => String::new(),
    };
    // listing path (lower case, '\') -> chunk index
    let mut shown: Vec<(String, usize, ChunkId)> = Vec::new();
    for (i, id, path) in edit.chunks() {
        let p = match path {
            Some(p) => {
                let rel = p.replace('/', "\\");
                if prefix.is_empty() { rel } else { format!("{prefix}\\{rel}") }
            }
            None => format!("{CHUNKS_DIR}\\{}.{}", id.hex(), id.kind(version).name()),
        };
        shown.push((p.to_lowercase(), i, id));
    }
    let mut removals = Vec::new();
    let mut refused = Vec::new();
    for item in items {
        let it = item.replace('/', "\\");
        let first = it.split('\\').next().unwrap_or("");
        if first.eq_ignore_ascii_case(LEGACY_DIR) {
            refused.push(it);
            continue;
        }
        let lower = it.to_lowercase();
        let matched: Vec<&(String, usize, ChunkId)> = match lower.strip_suffix("*.*") {
            Some(dir) => shown.iter().filter(|(p, _, _)| p.starts_with(dir)).collect(),
            None => shown.iter().filter(|(p, _, _)| *p == lower).collect(),
        };
        for (_, i, id) in matched {
            if id.kind(version) == ChunkKind::ContainerHeader {
                refused.push(it.clone());
            } else if !removals.contains(i) {
                removals.push(*i);
            }
        }
    }
    if !refused.is_empty() {
        return Err(PackError::User(format!("Nothing was deleted. Read-only ($legacy view, container header):\n  {}", refused.join("\n  "))));
    }
    if removals.is_empty() {
        return Err(PackError::User(format!("Nothing was deleted: these entries aren't chunks of the container:\n  {}", items.join("\n  "))));
    }
    if edit.is_signed() && !crate::dialog::confirm(&format!("{}\n\nThis container is signed. After the change it is written unsigned; a game that checks signatures may refuse it.\n\nContinue?", utoc.display())) {
        return Err(PackError::Aborted);
    }
    let (_, _, removed) = apply(edit, &utoc, &[], &removals, None, header, &HashMap::new())?;
    let compacted = maybe_compact(&utoc, &options.pack)?;
    Ok(Stats { replaced: 0, added: 0, removed, input_bytes: 0, compacted, new_container: false, pak_written: None })
}

// ---------------------------------------------------------------------------
// New containers

/// Mount point folders where the game has the packages dropped here (by the name inside them):
/// `../../../Moadra/Content/Mods/SML/` for `BP_SML.uasset` dropped alone.
fn suggested_mounts(inputs: &[Input], game: &Game, toc: TocVersion) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let (Some(toc_enum), Some(hv)) = (retoc::EIoStoreTocVersion::from_repr(toc.0), game_header_version(game)) else { return out };
    for input in inputs.iter().filter(|i| matches!(crate::add::ext_of(&i.archive_path.to_lowercase()).as_str(), "uasset" | "umap")).take(50) {
        let first = input.archive_path.split('\\').next().unwrap_or("");
        if first.eq_ignore_ascii_case(CHUNKS_DIR) || first.eq_ignore_ascii_case(LEGACY_DIR) || is_legacy_package(&input.src) {
            continue;
        }
        let Ok(h) = std::fs::File::open(&input.src).map_err(|e| e.to_string()).and_then(|f| crate::validate::read_header(&mut std::io::BufReader::new(f), None, toc_enum, hv)) else { continue };
        let Some(chunk) = ChunkId::create(crate::add::package_id(&h.package_name()), 0, ChunkKind::ExportBundleData, toc) else { continue };
        let Some(at) = game.by_id.get(&chunk).and_then(|fs| fs.iter().find_map(|f| f.full.as_deref())) else { continue };
        let at = crate::add::visible(at);
        let rel = input.archive_path.replace('\\', "/");
        if at.len() < rel.len() || !at[at.len() - rel.len()..].eq_ignore_ascii_case(&rel) {
            continue;
        }
        let folder = &at[..at.len() - rel.len()];
        // dropped with its whole path: Auto already fits
        if folder.is_empty() || !folder.ends_with('/') {
            continue;
        }
        let m = format!("../../../{folder}");
        if !out.iter().any(|o| o.eq_ignore_ascii_case(&m)) {
            out.push(m);
        }
    }
    out
}

/// What a mount point text means for the copied files (the dialog's preview line).
fn mount_preview(text: &str, inputs: &[Input], game: &Game) -> Result<String, String> {
    let mount = crate::pack_settings::normalize_mount_point(text)?;
    let in_game = |f: &str| game.by_path.contains_key(&full_key(f));
    let mut n = 0usize;
    let mut found = 0usize;
    let mut first = None;
    for input in inputs {
        let top = input.archive_path.split('\\').next().unwrap_or("");
        if top.eq_ignore_ascii_case(CHUNKS_DIR) || top.eq_ignore_ascii_case(LEGACY_DIR) {
            continue;
        }
        let rel = input.archive_path.replace('\\', "/");
        let full = new_container_full(&rel, mount.as_deref(), in_game);
        n += 1;
        if in_game(&full) {
            found += 1;
        }
        first.get_or_insert((rel, full));
    }
    let Some((rel, full)) = first else { return Ok(String::new()) };
    let more = if n > 1 { format!(" (and {} more)", n - 1) } else { String::new() };
    let full = if mount.is_none() { format!("{full} (the mount point becomes the files' common folder)") } else { full };
    Ok(format!("{rel} \u{2192} {full}{more}\nThe game has {found} of {n} at these paths (replaced in this container); the rest are new files."))
}

/// The mount point for a new container: asked (`AskMountPoint`) with the remembered ones, the
/// game's folders of the dropped packages and Auto; the answer is remembered first. `None` = Auto.
fn choose_new_mount(utoc: &Path, inputs: &[Input], game: &Game, toc: TocVersion, opts: &PackOptions) -> Result<Option<String>, PackError> {
    let mut choices: Vec<String> = opts.recent_mount_points.iter().take(opts.mount_point_history.max(1)).cloned().collect();
    let suggested = suggested_mounts(inputs, game, toc);
    for m in suggested.iter().chain(std::iter::once(&"Auto".to_string())) {
        if !choices.iter().any(|c| c.eq_ignore_ascii_case(m)) {
            choices.push(m.clone());
        }
    }
    let preset = opts.recent_mount_points.first().or(suggested.first()).cloned().unwrap_or_else(|| "Auto".into());
    let answer = if opts.ask_mount_point {
        let preview = |t: &str| mount_preview(t, inputs, game);
        let p = crate::dialog::MountPrompt { container: utoc, choices: &choices, preset: &preset, preview: &preview };
        crate::dialog::ask_mount_point(&p).ok_or(PackError::Aborted)?
    } else {
        preset
    };
    let mount = crate::pack_settings::normalize_mount_point(&answer).map_err(PackError::User)?;
    remember_mount(opts, mount.as_deref().unwrap_or("Auto"));
    Ok(mount)
}

/// Remembered mount points (tests): what would be written to `RecentMountPoints`.
#[cfg(test)]
pub static TEST_SAVED_MOUNTS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn remember_mount(opts: &PackOptions, used: &str) {
    if opts.mount_point_history == 0 {
        return;
    }
    let mut list = vec![used.to_string()];
    list.extend(opts.recent_mount_points.iter().filter(|r| !r.eq_ignore_ascii_case(used)).cloned());
    list.truncate(opts.mount_point_history);
    let value = list.join("|");
    #[cfg(test)]
    {
        *TEST_SAVED_MOUNTS.lock().unwrap() = list;
    }
    #[cfg(not(test))]
    if let Err(e) = crate::config::set_values("Pack", &[("RecentMountPoints", value.clone())]) {
        dlog!("  mount point not remembered: {e}");
    }
    dlog!("  RecentMountPoints={value}");
}

fn new_container_settings(utoc: &Path, game: &Game, steps: &[Step], mount: Option<&str>) -> Result<NewContainer, PackError> {
    let global = game.containers.iter().find(|c| c.is_global).ok_or_else(|| PackError::User("The global container couldn't be read.".into()))?;
    let others: Vec<&GameContainer> = game.containers.iter().filter(|c| !c.is_global).collect();
    let mut sizes: HashMap<u32, usize> = HashMap::new();
    for c in &others {
        *sizes.entry(c.block_size).or_default() += 1;
    }
    let block_size = sizes.into_iter().max_by_key(|x| (x.1, x.0)).map(|x| x.0).unwrap_or(global.block_size);
    let name = utoc.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mount_point = match mount {
        Some(m) => {
            let m = if m.ends_with('/') { m.to_string() } else { format!("{m}/") };
            // paths come from the mount point (new_container_full); game files copied by id keep
            // the game's path, which the lookup matched under the same rule
            for f in steps.iter().filter_map(Step::full) {
                if strip_prefix_ignore_case(f, &m).is_none() {
                    return Err(PackError::User(format!("{f} is not under the mount point {m}.")));
                }
            }
            m
        }
        None => {
            let fulls: Vec<&str> = steps.iter().filter_map(Step::full).collect();
            if fulls.is_empty() { "../../../".to_string() } else { iostore::common_root_path(fulls) }
        }
    };
    Ok(NewContainer {
        version: global.version,
        block_size,
        container_id: iostore::ContainerId(retoc::FIoContainerId::from_name(&name).0),
        mount_point,
        partition_size: None,
        key: None,
    })
}

/// An empty header in the game's container-header version (from any other container).
fn new_container_header(game: &Game, settings: &NewContainer) -> Result<Option<FIoContainerHeader>, PackError> {
    for c in game.containers.iter().filter(|c| !c.is_global) {
        let Ok(container) = open_quiet(&c.utoc) else { continue };
        if let Ok(Some((_, h))) = read_header(&container) {
            return Ok(Some(FIoContainerHeader::new(h.version, retoc::FIoContainerId(settings.container_id.0))));
        }
    }
    Err(PackError::User("No container of the game has a container header to take its version from.".into()))
}

/// An empty pak v11 (UnrealPak/repak layout: primary index + path hash index + full directory
/// index, each SHA-1 checked; 4.26+ readers accept it, every reader accepts older versions).
pub fn empty_pak() -> Vec<u8> {
    use sha1::Digest;
    let sha = |b: &[u8]| -> [u8; 20] { sha1::Sha1::digest(b).into() };
    let mount = b"../../../\0";
    let phi: Vec<u8> = [0u32.to_le_bytes(), 0u32.to_le_bytes()].concat(); // no entries, no pruned dirs
    let fdi: Vec<u8> = 0u32.to_le_bytes().to_vec(); // no directories
    let index_len = 4 + mount.len() + 4 + 8 + 4 + 36 + 4 + 36 + 4 + 4;
    let phi_offset = index_len as u64;
    let fdi_offset = phi_offset + phi.len() as u64;
    let mut index = Vec::with_capacity(index_len);
    index.extend((mount.len() as u32).to_le_bytes());
    index.extend(mount);
    index.extend(0u32.to_le_bytes()); // entries
    index.extend(0u64.to_le_bytes()); // path hash seed
    index.extend(1u32.to_le_bytes());
    index.extend(phi_offset.to_le_bytes());
    index.extend((phi.len() as u64).to_le_bytes());
    index.extend(sha(&phi));
    index.extend(1u32.to_le_bytes());
    index.extend(fdi_offset.to_le_bytes());
    index.extend((fdi.len() as u64).to_le_bytes());
    index.extend(sha(&fdi));
    index.extend(0u32.to_le_bytes()); // encoded entries
    index.extend(0u32.to_le_bytes()); // unencoded entries
    debug_assert_eq!(index.len(), index_len);
    let mut out = index.clone();
    out.extend(&phi);
    out.extend(&fdi);
    out.extend([0u8; 16]); // encryption key guid
    out.push(0); // index not encrypted
    out.extend(0x5A6F_12E1u32.to_le_bytes());
    out.extend(11u32.to_le_bytes());
    out.extend(0u64.to_le_bytes()); // index offset
    out.extend((index.len() as u64).to_le_bytes());
    out.extend(sha(&index));
    out.extend([0u8; 32 * 5]); // compression methods
    out
}

// ---------------------------------------------------------------------------
// Compact (rebuild without dead space)

fn maybe_compact(utoc: &Path, opts: &PackOptions) -> Result<bool, PackError> {
    let size = container_bytes(utoc);
    if !(opts.compact || (opts.auto_compact_under > 0 && size < opts.auto_compact_under)) {
        return Ok(false);
    }
    match compact(utoc) {
        Ok(()) => Ok(true),
        Err(e) => {
            // The change itself is done; a failed rebuild only leaves dead space.
            dlog!("  compact failed (container kept as changed): {e}");
            Ok(false)
        }
    }
}

/// Rebuilds the container into a fresh pair (raw blocks copied, no recompression), then swaps.
pub fn compact(utoc: &Path) -> Result<(), PackError> {
    let profile = crate::profile::profile_for(utoc);
    let mut c = Container::open(utoc, profile.clone())?;
    if c.is_encrypted() {
        match crate::read::resolve_key(&mut c, utoc) {
            Ok(()) => {}
            Err(_) => return Err(PackError::Aborted),
        }
    }
    let index = c.directory_index()?.clone();
    let mut paths: Vec<Option<String>> = vec![None; c.chunk_count() as usize];
    for f in &index.files {
        if let Some(p) = paths.get_mut(f.toc_index as usize) {
            *p = Some(f.path.clone());
        }
    }
    let h = c.header().clone();
    let stem = utoc.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let tmp = utoc.with_file_name(format!("{stem}.uetoc-new.utoc"));
    let settings = NewContainer {
        version: h.version,
        block_size: h.block_size,
        container_id: h.container_id,
        mount_point: index.mount_point.clone(),
        partition_size: (h.effective_partition_size() != u64::MAX).then_some(h.effective_partition_size()),
        key: c.key().map(|k| (k.clone(), h.key_guid)),
    };
    let mut edit = ContainerEdit::create(&tmp, &settings)?;
    let toc = c.toc().clone();
    let result = (|| -> Result<(), PackError> {
        for i in 0..c.chunk_count() {
            let blocks = toc.chunk_blocks(i);
            let mut it = blocks.map(|b| -> iostore::Result<_> {
                let e = toc.blocks[b as usize];
                let raw = c.read_raw_block(b)?;
                let method = (e.method > 0).then(|| toc.compression_methods[e.method as usize - 1].clone());
                Ok((raw, e, method))
            });
            edit.add_raw(toc.chunk_ids[i as usize], paths[i as usize].clone(), toc.offsets[i as usize].length, toc.metas[i as usize], &mut it)?;
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = edit.rollback();
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    edit.finish()?;
    let parts_new = (0..).map(|i| partition_path(&tmp, i)).take_while(|p| p.exists()).count();
    let parts_old = c.partitions().len();
    drop(c);
    // swap: old -> .uetoc-old, new -> final, delete old
    let old_name = |p: &Path| p.with_file_name(format!("{}.uetoc-old", p.file_name().unwrap().to_string_lossy()));
    let mut olds = vec![utoc.to_path_buf()];
    olds.extend((0..parts_old).map(|i| partition_path(utoc, i)));
    for p in &olds {
        if p.exists() {
            std::fs::rename(p, old_name(p))?;
        }
    }
    for i in 0..parts_new {
        std::fs::rename(partition_path(&tmp, i), partition_path(utoc, i))?;
    }
    std::fs::rename(&tmp, utoc)?;
    for p in &olds {
        let _ = std::fs::remove_file(old_name(p));
    }
    dlog!("  compacted: {} bytes", container_bytes(utoc));
    Ok(())
}

// ---------------------------------------------------------------------------
// Interrupted changes

/// On open: an undo file means a change was interrupted. Offers to restore (or discard).
pub fn check_pending_undo(utoc: &Path) {
    let undo = undo_path(utoc);
    if !undo.exists() || crate::busy::is_busy(utoc) {
        return;
    }
    let restore = crate::dialog::confirm(&format!(
        "{}\n\nAn earlier change to this container was interrupted.\n\nYes: restore the container as it was before that change.\nNo: keep it as it is (the interrupted change's data may be partly there).",
        utoc.display()
    ));
    let r = (|| -> Result<(), String> {
        if restore {
            let rec = UndoRecord::from_bytes(&std::fs::read(&undo).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            rec.restore(utoc).map_err(|e| e.to_string())?;
        }
        std::fs::remove_file(&undo).map_err(|e| e.to_string())
    })();
    match r {
        Ok(()) => dlog!("  pending undo {}", if restore { "restored" } else { "discarded" }),
        Err(e) => crate::win::error(&format!("{}\n\nThe interrupted change could not be resolved: {e}", utoc.display())),
    }
}

pub fn delete_sources(steps_inputs: &[PathBuf]) {
    for p in steps_inputs {
        if let Err(e) = std::fs::remove_file(p) {
            dlog!("  could not delete source {}: {e}", p.display());
        }
    }
}
