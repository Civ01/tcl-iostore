//! `ValidateOnReplace` (SPEC §8.5): checks a package that takes the place of one the game
//! already has (N1 replace, or a game file copied into another container) against the game's
//! version and its store entry, which packing reuses.
//!
//! The runtime loader resolves a package's imports through the store entry's imported-package
//! list (AsyncLoading2: the import map holds indices into it), so that list must match the new
//! package. Other packages import this one by public export hash, so removed or retyped public
//! exports break them. Checks:
//! - package name inside the file vs the game's
//! - imported packages vs the store entry (5.3+: the package names them, so the list can be
//!   rebuilt = the only fixable issue; before 5.3: indices beyond the list, export count)
//! - public exports removed, or their class changed, and which game packages import this one
//!
//! Plain data in and out; `pack.rs` reads the chunks, asks and reports.

use retoc::container_header::{EIoContainerHeaderVersion, StoreEntry};
use retoc::script_objects::{FPackageObjectIndex, FPackageObjectIndexType};
use retoc::zen::FZenPackageHeader;
use retoc::{EIoStoreTocVersion, FPackageId};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek};
use std::panic::AssertUnwindSafe;

/// Something about a replacement package that can break loading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Issue {
    /// The new file couldn't be read as a Zen package of this game.
    Unreadable(String),
    /// The package name stored in the file differs from the game's.
    NameChanged { old: String, new: String },
    /// Imported packages differ from the store entry. `fixable`: the file names its imports
    /// (5.3+), so the entry's list can be rebuilt.
    ImportsChanged { added: Vec<String>, removed: Vec<String>, reordered: bool, fixable: bool },
    /// Before 5.3: the import map points past the end of the store entry's list.
    ImportsOutOfRange { needed: usize, available: usize },
    /// Before 5.3: the store entry's export count differs from the file's.
    ExportCountChanged { old: i32, new: usize },
    /// Public exports of the game's version missing in the new file.
    ExportsRemoved { names: Vec<String> },
    /// Public exports whose class changed.
    ClassChanged { names: Vec<String> },
}

impl Issue {
    pub fn fixable(&self) -> bool {
        matches!(self, Issue::ImportsChanged { fixable: true, .. })
    }

    /// Whether other game packages that import this one can break (exports gone or retyped).
    pub fn affects_importers(&self) -> bool {
        matches!(self, Issue::ExportsRemoved { .. } | Issue::ClassChanged { .. })
    }

    pub fn describe(&self) -> String {
        let few = |v: &[String]| {
            let mut s = v.iter().take(4).cloned().collect::<Vec<_>>().join(", ");
            if v.len() > 4 {
                s.push_str(&format!(" … (+{})", v.len() - 4));
            }
            s
        };
        match self {
            Issue::Unreadable(e) => format!("can't be checked: {e}"),
            Issue::NameChanged { old, new } => format!("package name inside the file is {new}, the game's is {old}"),
            Issue::ImportsChanged { added, removed, reordered, .. } => {
                let mut parts = Vec::new();
                if !added.is_empty() {
                    parts.push(format!("imports added: {}", few(added)));
                }
                if !removed.is_empty() {
                    parts.push(format!("imports removed: {}", few(removed)));
                }
                if *reordered && added.is_empty() && removed.is_empty() {
                    parts.push("imports in a different order".into());
                }
                parts.join("; ")
            }
            Issue::ImportsOutOfRange { needed, available } => format!("uses {needed} imported packages, the game's entry lists {available}"),
            Issue::ExportCountChanged { old, new } => format!("{new} exports, the game's entry says {old}"),
            Issue::ExportsRemoved { names } => format!("public exports removed: {}", few(names)),
            Issue::ClassChanged { names } => format!("class changed: {}", few(names)),
        }
    }
}

/// Result for one package.
#[derive(Debug, Clone, Default)]
pub struct Check {
    pub issues: Vec<Issue>,
    /// The store entry with the fixable issues corrected (`None`: nothing to fix).
    pub fixed_entry: Option<StoreEntry>,
}

/// Reads a Zen package header; retoc panics on some malformed input, so that is caught too.
pub fn read_header<R: Read + Seek>(r: &mut R, entry: Option<StoreEntry>, toc: EIoStoreTocVersion, header: EIoContainerHeaderVersion) -> Result<FZenPackageHeader, String> {
    match std::panic::catch_unwind(AssertUnwindSafe(|| FZenPackageHeader::deserialize(r, entry, toc, header, None))) {
        Ok(Ok(h)) => Ok(h),
        Ok(Err(e)) => Err(format!("{e:#}")),
        Err(_) => Err("malformed package header".into()),
    }
}

/// Whether packages of this container-header version name their imports (UE 5.3+).
pub fn names_imports(header: EIoContainerHeaderVersion) -> bool {
    header >= EIoContainerHeaderVersion::NoExportInfo
}

/// Whether package imports are (imported package index, export hash index) pairs (UE 5.0+).
/// UE 4.27 stores a hash of the imported object's path instead, the same in every package.
pub fn imports_are_refs(header: EIoContainerHeaderVersion) -> bool {
    header > EIoContainerHeaderVersion::Initial
}

/// A class reference that means the same in two packages (indices into the imported-package
/// list differ when the list changes): script import path hash, (package id, export hash) of a
/// package import (4.27: its path hash), or the export's name for a class inside the package.
fn class_key(h: &FZenPackageHeader, idx: FPackageObjectIndex) -> String {
    match idx.kind() {
        FPackageObjectIndexType::PackageImport if !imports_are_refs(h.container_header_version) => format!("raw:{:x}", idx.to_raw()),
        FPackageObjectIndexType::Export => idx
            .export()
            .and_then(|i| h.export_map.get(i as usize))
            .map(|e| format!("export:{}", h.name_map.get(e.object_name)))
            .unwrap_or_else(|| format!("raw:{:x}", idx.to_raw())),
        FPackageObjectIndexType::PackageImport => match idx.package_import() {
            Some(r) => {
                let pkg = h.imported_packages.get(r.imported_package_index as usize).map(|p| p.0);
                let hash = h.imported_public_export_hashes.get(r.imported_public_export_hash_index as usize);
                match (pkg, hash) {
                    (Some(p), Some(x)) => format!("import:{p:016x}:{x:016x}"),
                    _ => format!("raw:{:x}", idx.to_raw()),
                }
            }
            None => format!("raw:{:x}", idx.to_raw()),
        },
        _ => format!("raw:{:x}", idx.to_raw()),
    }
}

fn package_label(id: FPackageId, names: &HashMap<FPackageId, String>) -> String {
    names.get(&id).cloned().unwrap_or_else(|| format!("{:016x}", id.0))
}

/// Compares the replacement `new` with the game's version `old` (read with `entry`).
/// `new` was read without the entry where the package names its imports (5.3+), with it before.
pub fn compare(old: &FZenPackageHeader, new: &FZenPackageHeader, entry: &StoreEntry, header: EIoContainerHeaderVersion) -> Check {
    let mut issues = Vec::new();
    let mut fixed_entry = None;

    let (old_name, new_name) = (old.package_name(), new.package_name());
    if !old_name.eq_ignore_ascii_case(&new_name) {
        issues.push(Issue::NameChanged { old: old_name, new: new_name });
    }

    if names_imports(header) {
        if new.imported_packages != entry.imported_packages {
            // the names list runs parallel to the ids (the ids are the names' hashes)
            let mut names: HashMap<FPackageId, String> = HashMap::new();
            if old.imported_package_names.len() == entry.imported_packages.len() {
                names.extend(entry.imported_packages.iter().copied().zip(old.imported_package_names.iter().cloned()));
            }
            if new.imported_package_names.len() == new.imported_packages.len() {
                names.extend(new.imported_packages.iter().copied().zip(new.imported_package_names.iter().cloned()));
            }
            let old_set: HashSet<FPackageId> = entry.imported_packages.iter().copied().collect();
            let new_set: HashSet<FPackageId> = new.imported_packages.iter().copied().collect();
            let added: Vec<String> = new.imported_packages.iter().filter(|p| !old_set.contains(p)).map(|p| package_label(*p, &names)).collect();
            let removed: Vec<String> = entry.imported_packages.iter().filter(|p| !new_set.contains(p)).map(|p| package_label(*p, &names)).collect();
            issues.push(Issue::ImportsChanged { added, removed, reordered: true, fixable: true });
            fixed_entry = Some(StoreEntry { imported_packages: new.imported_packages.clone(), ..entry.clone() });
        }
    } else if imports_are_refs(header) {
        let needed = new.import_map.iter().filter_map(|i| i.package_import()).map(|r| r.imported_package_index as usize + 1).max().unwrap_or(0);
        if needed > entry.imported_packages.len() {
            issues.push(Issue::ImportsOutOfRange { needed, available: entry.imported_packages.len() });
        }
    }
    // 4.27: imports are path hashes; which package each belongs to isn't in the file, so a new
    // import can't be told from an old one here.
    if !names_imports(header) && entry.export_count >= 0 && entry.export_count as usize != new.export_map.len() {
        issues.push(Issue::ExportCountChanged { old: entry.export_count, new: new.export_map.len() });
    }

    // public exports: by hash (4.27 writes a null object index, not 0, for exports without one)
    let public = |hash: u64| hash != 0 && (imports_are_refs(header) || !FPackageObjectIndex::create_from_raw(hash).is_null());
    let new_by_hash: HashMap<u64, String> = new.export_map.iter().filter(|e| public(e.public_export_hash)).map(|e| (e.public_export_hash, class_key(new, e.class_index))).collect();
    let mut removed = Vec::new();
    let mut retyped = Vec::new();
    for e in old.export_map.iter().filter(|e| public(e.public_export_hash)) {
        let name = old.name_map.get(e.object_name).to_string();
        match new_by_hash.get(&e.public_export_hash) {
            None => removed.push(name),
            Some(class) if *class != class_key(old, e.class_index) => retyped.push(name),
            _ => {}
        }
    }
    if !removed.is_empty() {
        issues.push(Issue::ExportsRemoved { names: removed });
    }
    if !retyped.is_empty() {
        issues.push(Issue::ClassChanged { names: retyped });
    }
    Check { issues, fixed_entry }
}

/// One package's findings, for the messages.
#[derive(Debug, Clone)]
pub struct Finding {
    pub file: String,
    pub issues: Vec<Issue>,
    /// Game packages that import this one (only looked up when exports are affected).
    pub importers: Vec<String>,
    pub importer_count: usize,
}

/// Text listing `findings` (at most 15 files). After a Fix (`fixed`) each issue is marked
/// `[fixed]` or `[not fixable]`; before packing, issues Fix would correct are marked `[fixable]`.
pub fn report(findings: &[Finding], fixed: bool) -> String {
    let mut s = String::new();
    for f in findings.iter().take(15) {
        s.push_str(&format!("\n{}\n", f.file));
        for i in &f.issues {
            let mark = match (fixed, i.fixable()) {
                (true, true) => "[fixed] ",
                (true, false) => "[not fixable] ",
                (false, true) => "[fixable] ",
                (false, false) => "",
            };
            s.push_str(&format!("  - {mark}{}\n", i.describe()));
        }
        if f.importer_count > 0 {
            let mut list = f.importers.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
            if f.importer_count > 3 {
                list.push_str(&format!(" … (+{})", f.importer_count - 3));
            }
            s.push_str(&format!("  imported by {} game package(s): {list}\n", f.importer_count));
        }
    }
    if findings.len() > 15 {
        s.push_str(&format!("\n… and {} more file(s) (see the log with DebugLog=1)\n", findings.len() - 15));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issues_describe_and_classify() {
        let i = Issue::ImportsChanged { added: vec!["/Game/A".into()], removed: vec![], reordered: true, fixable: true };
        assert!(i.fixable());
        assert_eq!(i.describe(), "imports added: /Game/A");
        let i = Issue::ImportsChanged { added: vec![], removed: vec![], reordered: true, fixable: false };
        assert!(!i.fixable());
        assert_eq!(i.describe(), "imports in a different order");
        assert!(Issue::ExportsRemoved { names: vec!["X".into()] }.affects_importers());
        assert!(!Issue::NameChanged { old: "a".into(), new: "b".into() }.fixable());
        let many: Vec<String> = (0..6).map(|i| format!("E{i}")).collect();
        assert_eq!(Issue::ClassChanged { names: many }.describe(), "class changed: E0, E1, E2, E3 … (+2)");
    }

    #[test]
    fn report_marks_fixes() {
        let f = Finding {
            file: "Game\\BP.uasset".into(),
            issues: vec![
                Issue::ImportsChanged { added: vec!["/Game/A".into()], removed: vec![], reordered: true, fixable: true },
                Issue::ExportsRemoved { names: vec!["Foo".into()] },
            ],
            importers: vec!["Game/Content/X.uasset".into()],
            importer_count: 1,
        };
        let r = report(std::slice::from_ref(&f), true);
        assert!(r.contains("[fixed] imports added: /Game/A"), "{r}");
        assert!(r.contains("[not fixable] public exports removed: Foo"), "{r}");
        assert!(r.contains("imported by 1 game package(s): Game/Content/X.uasset"), "{r}");
        let before = report(&[f], false);
        assert!(before.contains("[fixable] imports added") && !before.contains("[fixed]") && !before.contains("[not fixable]"), "{before}");
    }
}
