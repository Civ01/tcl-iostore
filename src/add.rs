//! N1b (SPEC §8.4): packing files the game doesn't have.
//!
//! - A new package's chunk id is its **internal** package name's id (the loader looks packages
//!   up by name), its store entry is built from its own `ImportedPackageNames` (UE 5.3+ only;
//!   older games are refused), with no shader maps and no soft references.
//! - Bulk files (`.ubulk`, `.uptnl`, `.m.ubulk`) are pure payload described by their package's
//!   bulk-data map: id = package id + kind, index 0; their package must be in the game or in the
//!   same copy.
//! - `ValidateOnAdd` checks the path against the internal name (path-based discovery: mod
//!   loaders, tools) and warns about materials (no shader maps); Fix moves files (with their bulk
//!   files) to the path their name gives.
//! - One question per copy lists the new files and, when needed, the mount-point change with a
//!   before/after example (as tcl-uepak).

use crate::config::ValidateOnAdd;
use crate::validate;
use iostore::{ChunkId, ChunkKind, TocVersion};
use retoc::container_header::{EIoContainerHeaderVersion, StoreEntry};
use retoc::script_objects::{FPackageObjectIndex, FPackageObjectIndexType};
use std::collections::HashMap;
use std::path::Path;

/// A file not in the game: its full game path (`../../../Game/Content/X.uasset`, case kept).
#[derive(Debug, Clone)]
pub struct Candidate {
    pub input: crate::pack::Input,
    pub full: String,
}

/// The visible part of a full path (`Game/Content/X.uasset` for `../../../Game/Content/X.uasset`).
pub fn visible(full: &str) -> &str {
    let mut s = full.trim_start_matches('/');
    loop {
        if let Some(r) = s.strip_prefix("../") {
            s = r;
        } else if let Some(r) = s.strip_prefix("./") {
            s = r;
        } else {
            return s;
        }
    }
}

fn base_of(full: &str) -> &str {
    &full[..full.len() - visible(full).len()]
}

/// The file's extension as UE names its chunk files (`m.ubulk` for memory-mapped bulk).
pub fn ext_of(path: &str) -> String {
    let l = path.to_ascii_lowercase();
    if l.ends_with(".m.ubulk") {
        return "m.ubulk".into();
    }
    l.rsplit_once('.').map(|x| x.1.to_string()).unwrap_or_default()
}

fn strip_ext(path: &str) -> &str {
    let l = path.to_ascii_lowercase();
    let n = if l.ends_with(".m.ubulk") { 8 } else { path.rfind('.').map(|i| path.len() - i).unwrap_or(0) };
    &path[..path.len() - n]
}

/// Mount root and content folder of a visible path: (`Game/Content`, `/Game`, rest after
/// `Content/`). `Engine/Content` → `/Engine`; `…/Plugins/…/<P>/Content` → `/<P>`; any other
/// single folder before `Content` → `/Game`. `None` when there's no `Content` folder or the
/// layout isn't one of those.
pub fn content_root(visible_path: &str) -> Option<(String, String, String)> {
    let segs: Vec<&str> = visible_path.split('/').collect();
    let ci = segs.iter().position(|s| s.eq_ignore_ascii_case("content"))?;
    let prefix = &segs[..ci];
    let root = if prefix.iter().any(|s| s.eq_ignore_ascii_case("plugins")) {
        format!("/{}", prefix.last()?)
    } else if prefix.len() == 1 && prefix[0].eq_ignore_ascii_case("engine") {
        "/Engine".to_string()
    } else if prefix.len() == 1 {
        "/Game".to_string()
    } else {
        return None;
    };
    Some((segs[..=ci].join("/"), root, segs[ci + 1..].join("/")))
}

/// The package name a file at `visible_path` is expected to have (`/Game/Mods/X`).
pub fn name_for_path(visible_path: &str) -> Option<String> {
    let (_, root, rest) = content_root(visible_path)?;
    Some(format!("{root}/{}", strip_ext(&rest)))
}

/// Where a package named `name` (with extension `ext`) belongs: the drop's own content folder
/// when its root matches, else a content folder of the game with that root.
pub fn path_for_name<'a>(name: &str, ext: &str, drop_visible: &str, game_visibles: impl Iterator<Item = &'a str>) -> Option<String> {
    let (root, rest) = name.trim_start_matches('/').split_once('/')?;
    let root = format!("/{root}");
    let prefix = match content_root(drop_visible) {
        Some((p, r, _)) if r.eq_ignore_ascii_case(&root) => p,
        _ => game_visibles.filter_map(content_root).find(|(_, r, _)| r.eq_ignore_ascii_case(&root)).map(|(p, _, _)| p)?,
    };
    Some(format!("{prefix}/{rest}.{ext}"))
}

/// The package id of a package name (UE: CityHash64 of the lower-case UTF-16 name; retoc's
/// `FIoContainerId::from_name` is the same hash).
pub fn package_id(name: &str) -> u64 {
    retoc::FIoContainerId::from_name(name).0
}

/// Chunk kind of a bulk file extension.
pub fn bulk_kind(ext: &str) -> Option<ChunkKind> {
    match ext {
        "ubulk" => Some(ChunkKind::BulkData),
        "uptnl" => Some(ChunkKind::OptionalBulkData),
        "m.ubulk" => Some(ChunkKind::MemoryMappedBulkData),
        _ => None,
    }
}

fn is_material(h: &retoc::zen::FZenPackageHeader) -> bool {
    let classes = ["/Script/Engine.Material", "/Script/Engine.MaterialInstanceConstant"].map(|p| FPackageObjectIndex::create_script_import(p).to_raw());
    h.export_map.iter().any(|e| e.outer_index.is_null() && e.class_index.kind() == FPackageObjectIndexType::ScriptImport && classes.contains(&e.class_index.to_raw()))
}

/// Something `ValidateOnAdd` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddIssue {
    /// The path doesn't give the package's internal name; `to` = where it belongs (visible path),
    /// when that is known (Fix moves it there).
    PathMismatch { name: String, to: Option<String> },
    /// A material: no shader maps can be built for it (its shader library isn't packed).
    Material,
}

impl AddIssue {
    pub fn fixable(&self) -> bool {
        matches!(self, AddIssue::PathMismatch { to: Some(_), .. })
    }
    pub fn describe(&self) -> String {
        match self {
            AddIssue::PathMismatch { name, to: Some(to) } => format!("its name is {name}: belongs at {to}"),
            AddIssue::PathMismatch { name, to: None } => format!("its name is {name}, which no folder of the game gives"),
            AddIssue::Material => "a material: packed without shader maps (its shader library isn't packed), so it may not render".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AddFinding {
    /// Visible path as dropped.
    pub file: String,
    pub issue: AddIssue,
}

/// New files, planned.
#[derive(Debug, Default)]
pub struct NewPlan {
    pub steps: Vec<crate::pack::Step>,
    pub findings: Vec<AddFinding>,
    /// (file, reason): nothing may be packed when this isn't empty.
    pub refused: Vec<(String, String)>,
    /// Files Fix moved (old visible path, new visible path).
    pub moved: Vec<(String, String)>,
}

/// The game's view needed to plan new files.
pub struct GameView<'a> {
    pub toc: TocVersion,
    pub header: Option<EIoContainerHeaderVersion>,
    /// Package id → its file somewhere in the game.
    pub package_in_game: &'a dyn Fn(u64) -> Option<String>,
    /// Lower-case visible path → package id of the game file there.
    pub id_at_path: &'a dyn Fn(&str) -> Option<u64>,
    /// Visible paths of game files (with case), to find content folders.
    pub visibles: &'a [String],
}

/// Plans `cands` (files the game doesn't have, not refused for their type).
pub fn plan_new(cands: &[Candidate], game: &GameView, mode: ValidateOnAdd) -> NewPlan {
    use crate::pack::{Op, Step};
    let mut plan = NewPlan::default();
    let toc = retoc::EIoStoreTocVersion::from_repr(game.toc.0);
    // packages first: bulk files need their ids
    struct Pkg {
        cand: Candidate,
        name: String,
        id: u64,
        entry: StoreEntry,
        to: Option<String>,
    }
    let mut pkgs: Vec<Pkg> = Vec::new();
    for c in cands.iter().filter(|c| matches!(ext_of(&c.full).as_str(), "uasset" | "umap")) {
        let shown = visible(&c.full).to_string();
        let Some(hv) = game.header.filter(|h| validate::names_imports(*h)) else {
            plan.refused.push((shown, "new packages need a UE 5.3+ game (older packages don't name their imports)".into()));
            continue;
        };
        let Some(toc) = toc else {
            plan.refused.push((shown, format!("TOC version {}", game.toc.0)));
            continue;
        };
        let header = std::fs::File::open(&c.input.src).map_err(|e| e.to_string()).and_then(|f| validate::read_header(&mut std::io::BufReader::new(f), None, toc, hv));
        let h = match header {
            Ok(h) => h,
            Err(e) => {
                plan.refused.push((shown, format!("not a Zen package of this game: {e}")));
                continue;
            }
        };
        let name = h.package_name();
        let id = package_id(&name);
        if let Some(at) = (game.package_in_game)(id) {
            plan.refused.push((shown, format!("the game already has {name} at {at}: drop it there to replace it")));
            continue;
        }
        let entry = StoreEntry { imported_packages: h.imported_packages.clone(), ..Default::default() };
        let mut to = None;
        if mode != ValidateOnAdd::Off {
            let expected = name_for_path(&shown);
            if expected.as_deref().is_none_or(|e| !e.eq_ignore_ascii_case(&name)) {
                let target = path_for_name(&name, &ext_of(&shown), &shown, game.visibles.iter().map(String::as_str));
                plan.findings.push(AddFinding { file: shown.clone(), issue: AddIssue::PathMismatch { name: name.clone(), to: target.clone() } });
                if mode == ValidateOnAdd::Fix {
                    to = target;
                }
            }
            if is_material(&h) {
                plan.findings.push(AddFinding { file: shown.clone(), issue: AddIssue::Material });
            }
        }
        pkgs.push(Pkg { cand: c.clone(), name, id, entry, to });
    }
    // bulk files: their package by path (same copy, then the game)
    for c in cands.iter().filter(|c| bulk_kind(&ext_of(&c.full)).is_some()) {
        let shown = visible(&c.full).to_string();
        let ext = ext_of(&c.full);
        let stem = strip_ext(&shown).to_ascii_lowercase();
        let in_copy = pkgs.iter().find(|p| strip_ext(visible(&p.cand.full)).eq_ignore_ascii_case(&stem));
        let (id, moved_to) = match in_copy {
            Some(p) => (Some(p.id), p.to.as_ref().map(|t| format!("{}.{ext}", strip_ext(t)))),
            None => (["uasset", "umap"].iter().find_map(|e| (game.id_at_path)(&format!("{stem}.{e}"))), None),
        };
        let Some(id) = id else {
            plan.refused.push((shown, "bulk data without its package (.uasset/.umap) in the game or in this copy".into()));
            continue;
        };
        let kind = bulk_kind(&ext).unwrap();
        let Some(chunk) = ChunkId::create(id, 0, kind, game.toc) else {
            plan.refused.push((shown, "no chunk type for bulk data in this TOC version".into()));
            continue;
        };
        let full = match &moved_to {
            Some(t) => {
                plan.moved.push((shown.clone(), t.clone()));
                format!("{}{t}", base_of(&c.full))
            }
            None => c.full.clone(),
        };
        plan.steps.push(Step { input: c.input.clone(), op: Op::New { full, entry: None, memory_mapped: kind == ChunkKind::MemoryMappedBulkData }, id: chunk });
    }
    for p in pkgs {
        let Some(chunk) = ChunkId::create(p.id, 0, ChunkKind::ExportBundleData, game.toc) else {
            plan.refused.push((visible(&p.cand.full).to_string(), "no package chunk type in this TOC version".into()));
            continue;
        };
        let full = match &p.to {
            Some(t) => {
                plan.moved.push((visible(&p.cand.full).to_string(), t.clone()));
                format!("{}{t}", base_of(&p.cand.full))
            }
            None => p.cand.full.clone(),
        };
        let _ = &p.name;
        plan.steps.push(Step { input: p.cand.input.clone(), op: Op::New { full, entry: Some(p.entry), memory_mapped: false }, id: chunk });
    }
    plan
}

fn list(s: &mut String, items: &[String]) {
    for f in items.iter().take(15) {
        s.push_str(&format!("  {f}\n"));
    }
    if items.len() > 15 {
        s.push_str(&format!("  … and {} more\n", items.len() - 15));
    }
}

/// The findings, one line each (`[fixed]`/`[fixable]` marks as ValidateOnReplace).
pub fn findings_text(f: &[AddFinding], fixed: bool) -> String {
    let lines: Vec<String> = f
        .iter()
        .map(|x| {
            let mark = match (fixed, x.issue.fixable()) {
                (true, true) => "[fixed] ",
                (false, true) => "[fixable] ",
                _ => "",
            };
            format!("{}: {mark}{}", x.file, x.issue.describe())
        })
        .collect();
    let mut s = String::new();
    list(&mut s, &lines);
    s
}

/// A mount-point change and an example, as tcl-uepak shows it.
pub struct MountChange<'a> {
    pub old: &'a str,
    pub new: &'a str,
    /// First existing entry (relative to the old mount point), if any.
    pub example_existing: Option<&'a str>,
    /// First new file (full path).
    pub example_new: Option<&'a str>,
}

/// The text of the mount-point block.
pub fn mount_text(m: &MountChange) -> String {
    let added = iostore::edit::strip_prefix_ignore_case(m.old, m.new).unwrap_or_default().to_string();
    let mut s = format!("These files go outside the container's mount point {}.\n\nTo fit them, the mount point becomes {}", m.old.trim_end_matches('/'), m.new.trim_end_matches('/'));
    if added.is_empty() {
        s.push_str(".\n");
    } else {
        s.push_str(&format!(", and \"{added}\" is added to the path of every file already in the container.\n"));
    }
    if let Some(e) = m.example_existing {
        s.push_str(&format!("\n  {e}  →  {added}{e}\n\nWhere the game finds them doesn't change: {}{e}\n", visible(m.old)));
    }
    if let Some(n) = m.example_new {
        s.push_str(&format!("Added files go to: {}\n", visible(n)));
    }
    s.push_str("\nOnly the container's index is rewritten; no file data is repacked.\n");
    s
}

/// The one question before anything is written (additions and/or a mount-point change).
pub fn question(utoc: &Path, new_files: &[String], existing: bool, findings: &str, moved: &[(String, String)], mount: Option<&str>) -> String {
    let mut s = format!("{}\n\n", utoc.display());
    if !new_files.is_empty() {
        if existing {
            s.push_str("Adding new files to an existing container may lead to unpredictable results.\n");
        } else {
            s.push_str("This new container gets files the game doesn't have. New files may lead to unpredictable results.\n");
        }
        s.push_str(&format!("\nNew files ({}):\n", new_files.len()));
        list(&mut s, new_files);
    }
    if !moved.is_empty() {
        s.push_str("\nMoved to the path their name gives (ValidateOnAdd=Fix):\n");
        let lines: Vec<String> = moved.iter().map(|(a, b)| format!("{a}  →  {b}")).collect();
        list(&mut s, &lines);
    }
    if !findings.is_empty() {
        s.push_str("\nFound (ValidateOnAdd):\n");
        s.push_str(findings);
    }
    if let Some(m) = mount {
        s.push('\n');
        s.push_str(m);
    }
    s.push_str("\nContinue?");
    s
}

/// The refusal text for `refused`.
pub fn refused_text(refused: &[(String, String)]) -> String {
    let mut by_reason: HashMap<&str, Vec<String>> = HashMap::new();
    let mut order = Vec::new();
    for (f, r) in refused {
        if !by_reason.contains_key(r.as_str()) {
            order.push(r.as_str());
        }
        by_reason.entry(r.as_str()).or_default().push(f.clone());
    }
    let mut s = String::from("Nothing was packed. These new files can't be added:\n");
    for r in order {
        s.push_str(&format!("\n{r}:\n"));
        list(&mut s, &by_reason[r]);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_names() {
        assert_eq!(visible("../../../Moadra/Content/Mods/X.uasset"), "Moadra/Content/Mods/X.uasset");
        assert_eq!(name_for_path("Moadra/Content/Mods/SML/BP_SML.uasset").as_deref(), Some("/Game/Mods/SML/BP_SML"));
        assert_eq!(name_for_path("Engine/Content/EngineMaterials/M.uasset").as_deref(), Some("/Engine/EngineMaterials/M"));
        assert_eq!(name_for_path("G1R/Plugins/G1R_DialogFacials_Foreign/Content/foreign/a.uasset").as_deref(), Some("/G1R_DialogFacials_Foreign/foreign/a"));
        assert_eq!(name_for_path("Engine/Plugins/FX/Niagara/Content/N.umap").as_deref(), Some("/Niagara/N"));
        assert_eq!(name_for_path("Moadra/Content/Movies/x.m.ubulk").as_deref(), Some("/Game/Movies/x"));
        assert_eq!(name_for_path("Moadra/Config/a.uasset"), None);
        assert_eq!(ext_of("A/b.M.UBULK"), "m.ubulk");
        assert_eq!(ext_of("A/b.uptnl"), "uptnl");
        // where a name belongs
        let game = ["Moadra/Content/A.uasset".to_string(), "Moadra/Plugins/Mods/SML2/Content/B.uasset".to_string()];
        let g = || game.iter().map(String::as_str);
        assert_eq!(path_for_name("/Game/Mods/SML/BP", "uasset", "Moadra/Content/Wrong/BP.uasset", g()).as_deref(), Some("Moadra/Content/Mods/SML/BP.uasset"));
        assert_eq!(path_for_name("/SML2/X", "uasset", "Moadra/Content/X.uasset", g()).as_deref(), Some("Moadra/Plugins/Mods/SML2/Content/X.uasset"));
        assert_eq!(path_for_name("/Unknown/X", "uasset", "Moadra/Content/X.uasset", g()), None);
        // UE's package id hash
        assert_eq!(package_id("/Game/Mods/SML/BP_SML"), package_id("/game/mods/sml/bp_sml"));
    }

    #[test]
    fn mount_block_reads_like_uepak() {
        let t = mount_text(&MountChange {
            old: "../../../Moadra/Content/Mods/SML/",
            new: "../../../Moadra/Content/",
            example_existing: Some("BP_SML.uasset"),
            example_new: Some("../../../Moadra/Content/Other/X.uasset"),
        });
        assert!(t.contains("outside the container's mount point ../../../Moadra/Content/Mods/SML."), "{t}");
        assert!(t.contains("becomes ../../../Moadra/Content, and \"Mods/SML/\" is added"), "{t}");
        assert!(t.contains("BP_SML.uasset  →  Mods/SML/BP_SML.uasset"), "{t}");
        assert!(t.contains("doesn't change: Moadra/Content/Mods/SML/BP_SML.uasset"), "{t}");
        assert!(t.contains("Added files go to: Moadra/Content/Other/X.uasset"), "{t}");
        let q = question(Path::new("C:/x.utoc"), &["Moadra/Content/Other/X.uasset".into()], true, "", &[], Some(&t));
        assert!(q.contains("Adding new files to an existing container may lead to unpredictable results.") && q.ends_with("Continue?"), "{q}");
        let r = refused_text(&[("a".into(), "r1".into()), ("b".into(), "r2".into()), ("c".into(), "r1".into())]);
        assert!(r.contains("r1:\n  a\n  c\n") && r.contains("r2:\n  b\n"), "{r}");
    }
}
