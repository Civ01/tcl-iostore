//! What TC sees behind an archive handle (SPEC §3–§5).

use std::path::{Path, PathBuf};

use iostore::Container;

use crate::config::{MountPointMode, Options};

/// Hidden folder at the panel root holding chunks the directory index doesn't name.
pub const CHUNKS_DIR: &str = "$chunks";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemKind {
    /// A folder of the mount point's visible part (`Folder` view).
    MountDir,
    /// The `$chunks` folder.
    ChunksDir,
    /// A chunk (TOC entry index).
    Chunk(u32),
    /// The generated `<ContainerId>.txt`.
    InfoFile,
    /// The `$legacy` folder.
    LegacyDir,
    /// A converted file in `$legacy`.
    Legacy(crate::legacy::LegacyKind),
}

#[derive(Debug, Clone)]
pub struct Item {
    /// Path shown to TC (`\`-separated).
    pub display: String,
    pub kind: ItemKind,
    pub pack: u64,
    pub unpack: u64,
}

impl Item {
    pub fn is_dir(&self) -> bool {
        matches!(self.kind, ItemKind::MountDir | ItemKind::ChunksDir | ItemKind::LegacyDir)
    }
}

pub struct Archive {
    /// The file TC opened (`.ucas` or `.utoc`).
    pub path: PathBuf,
    pub container: Container,
    pub items: Vec<Item>,
    next: usize,
    current: Option<usize>,
    /// The `.utoc`'s modification time, used for every entry.
    pub mtime: Option<std::time::SystemTime>,
    pub mount_folder_attr: u32,
    pub virtual_folder_attr: u32,
    /// Text of the info file (`.utoc` view).
    pub info: Option<Vec<u8>>,
    pub oodle_warned: bool,
    pub partition_warned: bool,
    /// A key prompt for data was cancelled on this handle.
    pub key_declined: bool,
    /// The global container's `.utoc` (found on open).
    pub global: Option<PathBuf>,
    /// Legacy conversion state, created on the first `$legacy` extraction.
    pub legacy: Option<crate::legacy::Session>,
    /// The legacy view could not be set up (shown once per handle).
    pub legacy_error: Option<String>,
    /// The engine-version prompt was cancelled on this handle.
    pub legacy_declined: bool,
    /// `LegacyEngineVersion` when the archive was opened (`None` = Auto).
    pub legacy_version: Option<retoc::version::EngineVersion>,
    pub legacy_guess_script_types: bool,
}

/// Rejects entry names that would escape the destination folder.
pub fn is_safe_relative(path: &str) -> bool {
    !path.is_empty() && !path.starts_with(['/', '\\']) && !path.contains(':') && path.split(['/', '\\']).all(|c| c != ".." && c != "." && !c.is_empty())
}

impl Archive {
    fn base(path: &Path, container: Container, options: &Options) -> Self {
        let mtime = std::fs::metadata(container.utoc_path()).and_then(|m| m.modified()).ok();
        Archive {
            path: path.to_path_buf(),
            container,
            items: Vec::new(),
            next: 0,
            current: None,
            mtime,
            mount_folder_attr: options.mount_folder_attr,
            virtual_folder_attr: options.virtual_folder_attr,
            info: None,
            oodle_warned: false,
            partition_warned: false,
            key_declined: false,
            global: None,
            legacy: None,
            legacy_error: None,
            legacy_declined: false,
            legacy_version: options.legacy_engine_version,
            legacy_guess_script_types: options.legacy_guess_script_types,
        }
    }

    /// `.utoc` view: one text file.
    pub fn info_view(path: &Path, container: Container, options: &Options, text: String) -> Self {
        let mut a = Self::base(path, container, options);
        let bytes = text.into_bytes();
        a.items.push(Item {
            display: format!("{}.txt", a.container.header().container_id),
            kind: ItemKind::InfoFile,
            pack: bytes.len() as u64,
            unpack: bytes.len() as u64,
        });
        a.info = Some(bytes);
        a
    }

    /// Full listing. The directory index must be decoded already (key set if needed).
    /// Returns the archive and the number of skipped (unsafe or invalid) index entries.
    pub fn listing(path: &Path, mut container: Container, options: &Options) -> iostore::Result<(Self, usize)> {
        let index = container.directory_index()?.clone();
        let mut a = Self::base(path, container, options);
        let c = &a.container;
        let prefix = match options.mount_point {
            MountPointMode::Root => String::new(),
            MountPointMode::Folder => crate::view::Mount::parse(&index.mount_point).display_prefix(),
        };
        let mut acc = String::new();
        for seg in prefix.split('\\').filter(|s| !s.is_empty()) {
            if !acc.is_empty() {
                acc.push('\\');
            }
            acc.push_str(seg);
            a.items.push(Item { display: acc.clone(), kind: ItemKind::MountDir, pack: 0, unpack: 0 });
        }
        let mut named = vec![false; c.chunk_count() as usize];
        let mut skipped = 0;
        let mut safe_files: Vec<(String, u32)> = Vec::new();
        for f in &index.files {
            let Ok(info) = c.chunk(f.toc_index) else {
                crate::dlog!("  skipped {:?}: TOC entry {} doesn't exist", f.path, f.toc_index);
                skipped += 1;
                continue;
            };
            if !is_safe_relative(&f.path) {
                crate::dlog!("  skipped unsafe path {:?}", f.path);
                skipped += 1;
                continue;
            }
            named[f.toc_index as usize] = true;
            safe_files.push((f.path.clone(), f.toc_index));
            let rel = f.path.replace('/', "\\");
            let display = if prefix.is_empty() { rel } else { format!("{prefix}\\{rel}") };
            a.items.push(Item { display, kind: ItemKind::Chunk(f.toc_index), pack: info.packed_size, unpack: info.size });
        }
        let unnamed: Vec<_> = c.chunks().filter(|i| !named[i.index as usize]).collect();
        if !unnamed.is_empty() {
            a.items.push(Item { display: CHUNKS_DIR.into(), kind: ItemKind::ChunksDir, pack: 0, unpack: 0 });
            for i in unnamed {
                a.items.push(Item {
                    display: format!("{CHUNKS_DIR}\\{}", chunk_file_name(&i, c.version())),
                    kind: ItemKind::Chunk(i.index),
                    pack: i.packed_size,
                    unpack: i.size,
                });
            }
        }
        // legacy view (SPEC §6a)
        let planned = crate::legacy::plan(c, &safe_files);
        if !planned.is_empty() {
            use crate::legacy::{Planned, LEGACY_DIR};
            a.items.push(Item { display: LEGACY_DIR.into(), kind: ItemKind::LegacyDir, pack: 0, unpack: 0 });
            let shown = |rel: &str| {
                let rel = rel.replace('/', "\\");
                if prefix.is_empty() { format!("{LEGACY_DIR}\\{rel}") } else { format!("{LEGACY_DIR}\\{prefix}\\{rel}") }
            };
            for p in planned {
                a.items.push(match p {
                    Planned::Legacy { rel, kind, size } => {
                        // scriptobjects.bin goes to the root, as retoc writes it
                        let display = if matches!(kind, crate::legacy::LegacyKind::ScriptObjects) { format!("{LEGACY_DIR}\\{rel}") } else { shown(&rel) };
                        Item { display, kind: ItemKind::Legacy(kind), pack: size, unpack: size }
                    }
                    Planned::Raw { rel, index, pack, size } => Item { display: shown(&rel), kind: ItemKind::Chunk(index), pack, unpack: size },
                });
            }
        }
        Ok((a, skipped))
    }

    pub fn file_count(&self) -> usize {
        self.items.iter().filter(|i| !i.is_dir()).count()
    }

    /// Advances to the next entry (ReadHeader). `None` at end of archive.
    pub fn next_item(&mut self) -> Option<&Item> {
        let idx = self.next;
        let item = self.items.get(idx)?;
        self.current = Some(idx);
        self.next += 1;
        Some(item)
    }

    pub fn current_item(&self) -> Option<&Item> {
        self.current.and_then(|i| self.items.get(i))
    }

    pub fn uses_oodle(&self) -> bool {
        self.container.compression_methods().contains(&iostore::Method::Oodle)
    }
}

/// `<24 hex chunk id>.<ChunkType>`, e.g. `b53fffed16aaad1300000006.ContainerHeader`.
pub fn chunk_file_name(i: &iostore::ChunkInfo, _version: iostore::TocVersion) -> String {
    format!("{}.{}", i.id.hex(), i.kind.name())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsafe_paths() {
        for bad in ["", "/a", "\\a", "a/../b", "c:/x", "a//b", "./a", "a/."] {
            assert!(!is_safe_relative(bad), "{bad}");
        }
        for ok in ["a", "Game/Content/a.uasset", "a..b/c"] {
            assert!(is_safe_relative(ok), "{ok}");
        }
    }
}
