//! Packing raw Zen files (N1, SPEC §6b).
//!
//! Every packed file must already exist in the game (`PackMode=ReplaceOnly`): in the target
//! container it is replaced in place (chunk ID, path and store entry stay); found only in another
//! container under the global's folder it is added to the target with that container's chunk ID,
//! path and (for packages) store entry. Anything else cancels the whole copy with one message.
//! New containers take their format from the game. The container edit itself is
//! [`iostore::edit::ContainerEdit`]: new data is appended, the `.utoc` replaced last, an undo file
//! kept while writing.

use crate::config::{MountPointMode, Options, PackCompression, PackMode, PackOptions};
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

fn open_quiet(utoc: &Path) -> Result<Container, String> {
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
        out.push(Input { src, archive_path, len: meta.len() });
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
    pub read_only: Vec<String>,
}

impl Refusals {
    pub fn is_empty(&self) -> bool {
        self.not_in_game.is_empty() && self.companion_pak.is_empty() && self.legacy.is_empty() && self.read_only.is_empty()
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
        list(&mut s, "Not in the game (only files the game already has can be packed)", &self.not_in_game);
        list(&mut s, "Belong in the companion .pak, not in IoStore", &self.companion_pak);
        list(&mut s, "Legacy cooked files (.uasset/.uexp): packing them comes later; use the raw Zen files", &self.legacy);
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
fn is_legacy_package(path: &Path) -> bool {
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

/// Classifies every input. `target` is the open edit (`None` for a new container).
pub fn plan(inputs: &[Input], game: &Game, utoc: &Path, target: Option<&ContainerEdit>, mode: MountPointMode) -> (Vec<Step>, Refusals) {
    let mut steps = Vec::new();
    let mut r = Refusals::default();
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
        let key = archive_game_key(&shown, mode, target_mount);
        let lower = key.to_ascii_lowercase();
        if (lower.ends_with(".uasset") || lower.ends_with(".umap") || lower.ends_with(".uexp")) && (lower.ends_with(".uexp") || is_legacy_package(&input.src)) {
            r.legacy.push(shown);
        } else if let Some(&(i, id)) = own.get(&key) {
            steps.push(Step { input: input.clone(), op: Op::Replace { index: i }, id });
        } else if let Some(f) = game.find_elsewhere(&key, utoc) {
            steps.push(Step { input: input.clone(), op: Op::Add { from: f.clone() }, id: f.id });
        } else if game.in_global(&key) {
            r.read_only.push(shown);
        } else if is_iostore_type(&key) {
            r.not_in_game.push(shown);
        } else {
            r.companion_pak.push(shown);
        }
    }
    (steps, r)
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
struct HeaderCache(HashMap<String, Option<FIoContainerHeader>>);

impl HeaderCache {
    fn get(&mut self, utoc: &Path) -> Result<Option<&FIoContainerHeader>, String> {
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
) -> Result<(usize, usize, usize), PackError> {
    let version = edit.toc().header.version;
    let undo = undo_path(utoc);
    std::fs::write(&undo, edit.undo_record()?.to_bytes())?;
    let result = (|| -> Result<(usize, usize, usize), PackError> {
        let mut header_changed = false;
        let mut cache = HeaderCache::default();
        let (mut replaced, mut added) = (0, 0);
        let target_mount = edit.mount_point().map(String::from);
        // mount point that covers every added file
        if let Some(m) = &target_mount {
            let fulls: Vec<&str> = steps.iter().filter_map(|s| if let Op::Add { from } = &s.op { from.full.as_deref() } else { None }).collect();
            if fulls.iter().any(|f| strip_prefix_ignore_case(f, m).is_none()) {
                let mut all: Vec<String> = edit.chunks().filter_map(|(_, _, p)| p.map(|p| format!("{m}{p}"))).collect();
                all.extend(fulls.iter().map(|s| s.to_string()));
                let root = iostore::common_root_path(all.iter().map(String::as_str));
                dlog!("  mount point {m:?} -> {root:?} (added files outside it)");
                edit.set_mount_point(&root)?;
            }
        }
        let mount = edit.mount_point().map(String::from);
        for s in steps {
            let mut f = std::fs::File::open(&s.input.src).map_err(|e| PackError::Failed(format!("{}: {e}", s.input.src.display())))?;
            let mut cancel = progress_for(&s.input.src);
            match &s.op {
                Op::Replace { index } => {
                    edit.replace(*index, &mut f, s.input.len, compressor, &mut cancel)?;
                    replaced += 1;
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
                        let entry = src.and_then(|h| h.get_store_entry(package));
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
    if opts.mode == PackMode::ReplaceWithAddition {
        dlog!("  PackMode=ReplaceWithAddition is not implemented yet: packing as ReplaceOnly");
    }
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
    let input_bytes: u64 = inputs.iter().map(|i| i.len).sum();
    if exists {
        let (edit, header) = open_edit(&utoc)?;
        let (steps, refusals) = plan(&inputs, &game, &utoc, Some(&edit), options.mount_point);
        if !refusals.is_empty() {
            return Err(PackError::User(refusals.message(&game.unreadable)));
        }
        if edit.is_signed() && !crate::dialog::confirm(&format!("{}\n\nThis container is signed. After the change it is written unsigned; a game that checks signatures may refuse it.\n\nContinue?", utoc.display())) {
            return Err(PackError::Aborted);
        }
        let methods = edit.toc().compression_methods.clone();
        let compressor = choose_compressor(opts, Some(&methods), &game)?;
        dlog!("  {} steps ({} replace, {} add), compression {compressor:?}", steps.len(), steps.iter().filter(|s| matches!(s.op, Op::Replace { .. })).count(), steps.iter().filter(|s| matches!(s.op, Op::Add { .. })).count());
        check_space(utoc.parent().unwrap_or(Path::new(".")), input_bytes)?;
        let (replaced, added, removed) = apply(edit, &utoc, &steps, &[], compressor.as_ref(), header)?;
        let compacted = maybe_compact(&utoc, opts)?;
        Ok(Stats { replaced, added, removed, input_bytes, compacted, new_container: false, pak_written: None })
    } else {
        let (steps, refusals) = plan(&inputs, &game, &utoc, None, options.mount_point);
        if !refusals.is_empty() {
            return Err(PackError::User(refusals.message(&game.unreadable)));
        }
        let settings = new_container_settings(&utoc, &game, &steps, opts)?;
        let compressor = choose_compressor(opts, None, &game)?;
        let header = new_container_header(&game, &settings)?;
        dlog!("  new container: TOC {} block {} mount {:?} id {} compression {compressor:?}", settings.version.0, settings.block_size, settings.mount_point, settings.container_id);
        check_space(utoc.parent().unwrap_or(Path::new(".")), input_bytes)?;
        let edit = ContainerEdit::create(&utoc, &settings)?;
        // a new container has a header from the start (written on the first package)
        let header = header.map(|h| (None, h));
        let (_, added, _) = apply(edit, &utoc, &steps, &[], compressor.as_ref(), header).inspect_err(|_| {
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
    let (_, _, removed) = apply(edit, &utoc, &[], &removals, None, header)?;
    let compacted = maybe_compact(&utoc, &options.pack)?;
    Ok(Stats { replaced: 0, added: 0, removed, input_bytes: 0, compacted, new_container: false, pak_written: None })
}

// ---------------------------------------------------------------------------
// New containers

fn new_container_settings(utoc: &Path, game: &Game, steps: &[Step], opts: &PackOptions) -> Result<NewContainer, PackError> {
    let global = game.containers.iter().find(|c| c.is_global).ok_or_else(|| PackError::User("The global container couldn't be read.".into()))?;
    let others: Vec<&GameContainer> = game.containers.iter().filter(|c| !c.is_global).collect();
    let mut sizes: HashMap<u32, usize> = HashMap::new();
    for c in &others {
        *sizes.entry(c.block_size).or_default() += 1;
    }
    let block_size = sizes.into_iter().max_by_key(|x| (x.1, x.0)).map(|x| x.0).unwrap_or(global.block_size);
    let name = utoc.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mount_point = match &opts.new_mount_point {
        Some(m) => {
            let m = if m.ends_with('/') { m.clone() } else { format!("{m}/") };
            for s in steps {
                if let Op::Add { from: GameFile { full: Some(f), .. } } = &s.op {
                    if strip_prefix_ignore_case(f, &m).is_some() {
                        continue;
                    }
                    return Err(PackError::User(format!("{f} is not under the mount point {m} ([Pack] NewMountPoint).")));
                }
            }
            m
        }
        None => {
            let fulls: Vec<&str> = steps.iter().filter_map(|s| if let Op::Add { from } = &s.op { from.full.as_deref() } else { None }).collect();
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
