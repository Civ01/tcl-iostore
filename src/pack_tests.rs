//! Packing tests (SPEC §6b) on copies of whole sample game folders (`IOSTORE_SAMPLES`).
//! A game folder = a folder with `global.utoc` and at least one other container; encrypted and
//! trimmed ones are left out, as are games over 64 MB.

use crate::config::{MountPointMode, Options};
use crate::pack::{self, PackError};
use crate::view::Mount;
use iostore::{ChunkKind, Container, Vanilla};
use retoc::container_header::FIoContainerHeader;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn lock() -> std::sync::MutexGuard<'static, ()> {
    super::tests::lock()
}

fn games() -> Vec<PathBuf> {
    let Some(root) = std::env::var_os("IOSTORE_SAMPLES") else { return vec![] };
    if let Some(o) = std::env::var_os("IOSTORE_OODLE") {
        iostore::set_oodle_search_paths([PathBuf::from(o)]);
    }
    let oodle = iostore::oodle_status().is_ok();
    let mut out = Vec::new();
    let mut stack = vec![PathBuf::from(root)];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let mut utocs = Vec::new();
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "utoc") {
                utocs.push(p);
            }
        }
        if !utocs.iter().any(|p| p.file_name().is_some_and(|n| n == "global.utoc")) || utocs.len() < 2 {
            continue;
        }
        let ok = utocs.iter().all(|u| {
            let Ok(c) = Container::open(u, Arc::new(Vanilla)) else { return false };
            !c.is_encrypted() && !u.with_extension("trim.json").exists() && (oodle || !c.toc().compression_methods.iter().any(|m| m.eq_ignore_ascii_case("oodle")))
        });
        let size: u64 = std::fs::read_dir(&d).unwrap().flatten().filter_map(|e| e.metadata().ok()).filter(|m| m.is_file()).map(|m| m.len()).sum();
        if ok && size < 64 << 20 {
            out.push(d);
        }
    }
    out.sort();
    out
}

fn copy_game(src: &Path, name: &str) -> PathBuf {
    let dst = std::env::temp_dir().join("uetoc-pack-tests").join(name);
    let _ = std::fs::remove_dir_all(&dst);
    std::fs::create_dir_all(&dst).unwrap();
    for e in std::fs::read_dir(src).unwrap().flatten() {
        let p = e.path();
        let n = p.file_name().unwrap().to_string_lossy().to_lowercase();
        if p.is_file() && (n.ends_with(".utoc") || n.ends_with(".ucas") || n.ends_with(".pak")) {
            std::fs::copy(&p, dst.join(p.file_name().unwrap())).unwrap();
        }
    }
    dst
}

fn options(compact: bool) -> Options {
    let mut o = Options { mount_point: MountPointMode::Folder, ..Options::default() };
    o.pack.compact = false;
    o.pack.auto_compact_under = if compact { u64::MAX } else { 0 };
    o
}

/// Chunk data by id, listing (game path -> id), store entries.
struct Snapshot {
    chunks: BTreeMap<[u8; 12], Vec<u8>>,
    files: BTreeMap<String, [u8; 12]>,
    header: Option<FIoContainerHeader>,
    version: iostore::TocVersion,
}

fn snapshot(utoc: &Path) -> Snapshot {
    let mut c = Container::open(utoc, Arc::new(Vanilla)).unwrap();
    let mut chunks = BTreeMap::new();
    let mut header = None;
    for i in c.chunks().collect::<Vec<_>>() {
        let data = c.read_chunk(i.index).unwrap();
        if i.kind == ChunkKind::ContainerHeader {
            header = Some(FIoContainerHeader::deserialize(&mut std::io::Cursor::new(&data), None).unwrap());
        }
        chunks.insert(i.id.0, data);
    }
    let idx = c.directory_index().unwrap().clone();
    let ids = c.toc().chunk_ids.clone();
    let files = idx.files.iter().map(|f| (pack::game_key(&idx.mount_point, &f.path), ids[f.toc_index as usize].0)).collect();
    let version = c.version();
    Snapshot { chunks, files, header, version }
}

/// Writes every named file of `utoc` raw into `dir` under its listing path; returns the paths.
fn extract_named(utoc: &Path, dir: &Path) -> Vec<String> {
    let mut c = Container::open(utoc, Arc::new(Vanilla)).unwrap();
    let idx = c.directory_index().unwrap().clone();
    let prefix = Mount::parse(&idx.mount_point).display_prefix();
    let mut items = Vec::new();
    for f in &idx.files {
        let rel = f.path.replace('/', "\\");
        let shown = if prefix.is_empty() { rel } else { format!("{prefix}\\{rel}") };
        let out = dir.join(shown.replace('\\', "/"));
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        std::fs::write(&out, c.read_chunk(f.toc_index).unwrap()).unwrap();
        items.push(shown);
    }
    items
}

fn targets(game: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(game).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "utoc") && p.file_name().unwrap() != "global.utoc").collect();
    v.sort();
    v
}

#[test]
fn replace_every_file_with_itself() {
    let _l = lock();
    let games = games();
    if games.is_empty() {
        return eprintln!("IOSTORE_SAMPLES not set or no game folders: skipped");
    }
    for (n, g) in games.iter().enumerate() {
        for compact in [false, true] {
            let copy = copy_game(g, &format!("replace{n}{compact}"));
            for utoc in targets(&copy) {
                let before = snapshot(&utoc);
                let old_toc = std::fs::read(&utoc).unwrap();
                let src = copy.join("src").join(utoc.file_stem().unwrap());
                let items = extract_named(&utoc, &src);
                if items.is_empty() {
                    continue;
                }
                let st = pack::pack_files(&utoc.with_extension("ucas"), None, &src, &items, pack::PK_PACK_SAVE_PATHS, &options(compact)).unwrap_or_else(|e| panic!("{}: {e}", utoc.display()));
                assert_eq!((st.replaced, st.added, st.compacted), (items.len(), 0, compact), "{}", utoc.display());
                let after = snapshot(&utoc);
                assert_eq!(before.chunks, after.chunks, "{}", utoc.display());
                assert_eq!(before.files, after.files, "{}", utoc.display());
                assert_eq!(before.header, after.header, "{}", utoc.display());
                if !compact {
                    let a = iostore::Toc::parse(&old_toc, &Vanilla).unwrap();
                    let b = iostore::Toc::parse(&std::fs::read(&utoc).unwrap(), &Vanilla).unwrap();
                    assert_eq!(a.chunk_ids, b.chunk_ids);
                    assert_eq!(a.directory_index, b.directory_index);
                    assert_eq!(a.perfect_hash_seeds, b.perfect_hash_seeds);
                }
                assert!(!pack::undo_path(&utoc).exists());
                eprintln!("replace{}: {} ({} files)", if compact { "+compact" } else { "" }, utoc.display(), items.len());
            }
        }
    }
}

#[test]
fn add_from_another_container_and_new_container() {
    let _l = lock();
    let games: Vec<_> = games().into_iter().filter(|g| targets(g).len() >= 2).collect();
    if games.is_empty() {
        return eprintln!("no game folder with two containers: skipped");
    }
    for (n, g) in games.iter().enumerate() {
        let copy = copy_game(g, &format!("add{n}"));
        let t = targets(&copy);
        let (a, b) = (&t[0], &t[1]);
        let sa = snapshot(a);
        let sb = snapshot(b);
        // a package of A that B lacks (with its bulk data, if any)
        let Some((key, id)) = sa.files.iter().find(|(k, id)| k.ends_with(".uasset") && !sb.files.contains_key(*k) && !sb.chunks.contains_key(*id)).map(|(k, id)| (k.clone(), *id)) else { continue };
        let src = copy.join("src-add");
        let mut c = Container::open(a, Arc::new(Vanilla)).unwrap();
        let idx = c.directory_index().unwrap().clone();
        let prefix = Mount::parse(&idx.mount_point).display_prefix();
        let f = idx.files.iter().find(|f| pack::game_key(&idx.mount_point, &f.path) == key).unwrap().clone();
        let shown = format!("{prefix}\\{}", f.path.replace('/', "\\"));
        let out = src.join(shown.replace('\\', "/"));
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        std::fs::write(&out, c.read_chunk(f.toc_index).unwrap()).unwrap();
        drop(c);

        // into B
        let st = pack::pack_files(&b.with_extension("ucas"), None, &src, std::slice::from_ref(&shown), pack::PK_PACK_SAVE_PATHS, &options(false)).unwrap_or_else(|e| panic!("{}: {e}", b.display()));
        assert_eq!((st.replaced, st.added), (0, 1));
        let after = snapshot(b);
        assert_eq!(after.chunks.len(), sb.chunks.len() + 1);
        assert_eq!(after.chunks[&id], sa.chunks[&id]);
        assert_eq!(after.files.get(&key), Some(&id));
        let package = retoc::FPackageId(u64::from_le_bytes(id[..8].try_into().unwrap()));
        assert_eq!(after.header.as_ref().unwrap().get_store_entry(package), sa.header.as_ref().unwrap().get_store_entry(package), "store entry copied");
        for (k, v) in &sb.chunks {
            if iostore::ChunkId(*k).kind(sb.version) != ChunkKind::ContainerHeader {
                assert_eq!(after.chunks.get(k), Some(v), "chunk {}", iostore::ChunkId(*k).hex());
            }
        }

        // a new container with the same file (+ empty companion .pak)
        let newc = copy.join("zzz_test_P.utoc");
        let st = pack::pack_files(&newc.with_extension("ucas"), None, &src, std::slice::from_ref(&shown), pack::PK_PACK_SAVE_PATHS, &options(false)).unwrap_or_else(|e| panic!("new: {e}"));
        assert!(st.new_container && st.added == 1);
        let sn = snapshot(&newc);
        assert_eq!(sn.chunks[&id], sa.chunks[&id]);
        assert_eq!(sn.files.get(&key), Some(&id));
        assert_eq!(sn.header.as_ref().unwrap().get_store_entry(package), sa.header.as_ref().unwrap().get_store_entry(package));
        assert_eq!(sn.header.as_ref().unwrap().container_id.0, retoc::FIoContainerId::from_name("zzz_test_P").0);
        let pak = newc.with_extension("pak");
        assert!(pak.is_file());
        let r = repak::PakBuilder::new().reader(&mut std::io::BufReader::new(std::fs::File::open(&pak).unwrap())).expect("repak reads the empty pak");
        assert_eq!(r.files().len(), 0);
        assert_eq!(r.mount_point(), "../../../");
        // the new container opens in the plugin listing
        let arc = crate::read::open_archive(&newc.with_extension("ucas"), &options(false), true).map_err(|c| format!("open {c}")).unwrap();
        assert!(arc.items.iter().any(|i| i.display.to_lowercase().replace('\\', "/") == key));

        // delete it from B again: chunk and store entry gone
        let st = pack::delete_files(&b.with_extension("ucas"), std::slice::from_ref(&shown), &options(false)).unwrap();
        assert_eq!(st.removed, 1);
        let back = snapshot(b);
        assert!(!back.chunks.contains_key(&id));
        assert!(back.header.as_ref().unwrap().get_store_entry(package).is_none());
        assert_eq!(back.files, sb.files);
        eprintln!("add/new/delete: {} -> {}", a.display(), b.display());
    }
}

#[test]
fn refusals_leave_the_container_untouched() {
    let _l = lock();
    let Some(g) = games().into_iter().next() else { return eprintln!("skipped") };
    let copy = copy_game(&g, "refuse");
    let utoc = targets(&copy).remove(0);
    let before = std::fs::read(&utoc).unwrap();
    let ucas_len = std::fs::metadata(utoc.with_extension("ucas")).unwrap().len();
    let src = copy.join("src-refuse");
    std::fs::create_dir_all(&src).unwrap();
    let mut legacy = 0x9E2A_83C1u32.to_le_bytes().to_vec();
    legacy.extend([0u8; 64]);
    std::fs::write(src.join("Legacy.uasset"), &legacy).unwrap();
    std::fs::write(src.join("New.uasset"), b"zen?").unwrap();
    std::fs::write(src.join("DefaultGame.ini"), b"[x]").unwrap();
    let items = ["Legacy.uasset", "New.uasset", "DefaultGame.ini"].map(String::from);
    match pack::pack_files(&utoc.with_extension("ucas"), Some("Game\\Content"), &src, &items, pack::PK_PACK_SAVE_PATHS, &options(false)) {
        Err(PackError::User(msg)) => {
            assert!(msg.contains("Not in the game") && msg.contains("companion .pak") && msg.contains("Legacy cooked"), "{msg}");
        }
        other => panic!("expected a refusal, got {:?}", other.err()),
    }
    // $legacy and the container header
    std::fs::create_dir_all(src.join("$legacy")).unwrap();
    std::fs::write(src.join("$legacy").join("a.uasset"), b"x").unwrap();
    assert!(matches!(pack::pack_files(&utoc.with_extension("ucas"), None, &src, &["$legacy\\a.uasset".to_string()], pack::PK_PACK_SAVE_PATHS, &options(false)), Err(PackError::User(_))));
    assert_eq!(std::fs::read(&utoc).unwrap(), before);
    assert_eq!(std::fs::metadata(utoc.with_extension("ucas")).unwrap().len(), ucas_len);
    // the global
    let global = copy.join("global.utoc");
    assert!(matches!(pack::pack_files(&global.with_extension("ucas"), None, &src, &["New.uasset".to_string()], pack::PK_PACK_SAVE_PATHS, &options(false)), Err(PackError::User(_))));
}

#[test]
fn interrupted_change_is_restored_on_open() {
    let _l = lock();
    let Some(g) = games().into_iter().next() else { return eprintln!("skipped") };
    let copy = copy_game(&g, "undo");
    let utoc = targets(&copy).remove(0);
    let before = std::fs::read(&utoc).unwrap();
    let ucas_len = std::fs::metadata(utoc.with_extension("ucas")).unwrap().len();
    // half a change: data appended, undo written, .utoc rewritten, then "crash"
    let mut edit = iostore::edit::ContainerEdit::open(&utoc, &Vanilla, None).unwrap();
    std::fs::write(pack::undo_path(&utoc), edit.undo_record().unwrap().to_bytes()).unwrap();
    let data = vec![7u8; 100_000];
    let first = edit.chunks().next().unwrap().0;
    edit.replace(first, &mut data.as_slice(), data.len() as u64, None, &mut |_| false).unwrap();
    edit.finish().unwrap();
    assert_ne!(std::fs::read(&utoc).unwrap(), before);
    crate::dialog::TEST_CONFIRMS.lock().unwrap().push(true);
    pack::check_pending_undo(&utoc);
    assert_eq!(std::fs::read(&utoc).unwrap(), before);
    assert_eq!(std::fs::metadata(utoc.with_extension("ucas")).unwrap().len(), ucas_len);
    assert!(!pack::undo_path(&utoc).exists());
}

#[test]
fn empty_pak_is_what_repak_reads() {
    let p = pack::empty_pak();
    assert_eq!(p.len(), 347);
    let r = repak::PakBuilder::new().reader(&mut std::io::Cursor::new(&p)).unwrap();
    assert_eq!(r.version(), repak::Version::V11);
    assert!(r.files().is_empty());
}

/// Every package of a whole game compared with itself (`VALIDATE_SELF` = a global.utoc, e.g.
/// The Isle or ARK server): `ValidateOnReplace` must find nothing in an unmodified game.
#[test]
fn validation_finds_nothing_in_an_unmodified_game() {
    let Some(global) = std::env::var_os("VALIDATE_SELF").map(PathBuf::from) else { return eprintln!("VALIDATE_SELF not set: skipped") };
    if let Some(o) = std::env::var_os("IOSTORE_OODLE") {
        iostore::set_oodle_search_paths([PathBuf::from(o)]);
    }
    // key.txt next to the containers, for encrypted games
    for e in std::fs::read_dir(global.parent().unwrap()).unwrap().flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "utoc") && Container::open(&p, Arc::new(Vanilla)).is_ok_and(|c| c.is_encrypted()) {
            crate::tests::register_key(&p);
        }
    }
    let game = pack::Game::build(&global);
    let (mut checked, mut skipped) = (0usize, 0usize);
    let mut problems = Vec::new();
    for gc in game.containers.iter().filter(|c| !c.is_global) {
        let Ok(c) = pack::open_quiet(&gc.utoc) else {
            skipped += 1;
            continue;
        };
        let Some(hi) = c.chunks().find(|i| i.kind == ChunkKind::ContainerHeader) else { continue };
        let Ok(raw) = c.read_chunk(hi.index) else {
            skipped += 1;
            continue;
        };
        let header = FIoContainerHeader::deserialize(&mut std::io::Cursor::new(raw), None).unwrap();
        let toc = retoc::EIoStoreTocVersion::from_repr(gc.version.0).unwrap();
        for info in c.chunks().filter(|i| i.kind == ChunkKind::ExportBundleData).collect::<Vec<_>>() {
            let Some(entry) = header.get_store_entry(retoc::FPackageId(info.id.id64())) else { continue };
            let data = match c.read_chunk(info.index) {
                Ok(d) => d,
                Err(e) => {
                    problems.push(format!("{} {}: read {e}", gc.utoc.display(), info.id.hex()));
                    continue;
                }
            };
            let hv = header.version;
            let old = crate::validate::read_header(&mut std::io::Cursor::new(&data), Some(entry.clone()), toc, hv);
            let new_entry = if crate::validate::names_imports(hv) { None } else { Some(entry.clone()) };
            let new = crate::validate::read_header(&mut std::io::Cursor::new(&data), new_entry, toc, hv);
            match (old, new) {
                (Ok(o), Ok(n)) => {
                    let ch = crate::validate::compare(&o, &n, &entry, hv);
                    if !ch.issues.is_empty() {
                        problems.push(format!("{} {}: {:?}", gc.utoc.display(), info.id.hex(), ch.issues));
                    }
                }
                (o, n) => problems.push(format!("{} {}: unreadable {:?} / {:?}", gc.utoc.display(), info.id.hex(), o.err(), n.err())),
            }
            checked += 1;
        }
    }
    eprintln!("validation self-check: {checked} packages, {skipped} containers skipped, {} problems", problems.len());
    for p in problems.iter().take(20) {
        eprintln!("  {p}");
    }
    assert!(problems.is_empty());
}

/// `ValidateOnReplace` on a package replaced by a different one (B's path, A's bytes): every
/// mode does what SPEC §6b.1 says, in every sample game.
#[test]
fn validate_on_replace_modes() {
    use crate::config::ValidateOnReplace as V;
    let _l = lock();
    let games = games();
    if games.is_empty() {
        return eprintln!("IOSTORE_SAMPLES not set or no game folders: skipped");
    }
    let mut fixable_seen = false;
    for (n, g) in games.iter().enumerate() {
        let probe = copy_game(g, &format!("validate{n}"));
        let Some((utoc, a, b)) = targets(&probe).into_iter().find_map(|t| {
            let s = snapshot(&t);
            let pk: Vec<&String> = s.files.iter().filter(|(k, id)| k.ends_with(".uasset") && iostore::ChunkId(**id).kind(s.version) == ChunkKind::ExportBundleData).map(|(k, _)| k).collect();
            (pk.len() >= 2).then(|| (t.clone(), pk[0].clone(), pk[1].clone()))
        }) else {
            continue;
        };
        let name = utoc.file_name().unwrap().to_owned();
        let header_version = snapshot(&utoc).header.as_ref().unwrap().version;
        let fixable = crate::validate::names_imports(header_version);
        for mode in [V::Error, V::Warn, V::Warn, V::Fix, V::Off] {
            // fresh copy per mode
            let copy = copy_game(g, &format!("validate{n}-{mode:?}"));
            let utoc = copy.join(&name);
            let src = copy.join("src");
            let items = extract_named(&utoc, &src);
            let item_of = |key: &str| items.iter().find(|i| i.to_lowercase().replace('\\', "/").ends_with(key.split_once('/').map_or(key, |x| x.1))).unwrap().clone();
            let (ia, ib) = (item_of(&a), item_of(&b));
            std::fs::copy(src.join(ia.replace('\\', "/")), src.join(ib.replace('\\', "/"))).unwrap();
            let before = snapshot(&utoc);
            let package_b = retoc::FPackageId(u64::from_le_bytes(before.files[&b][..8].try_into().unwrap()));
            let package_a = retoc::FPackageId(u64::from_le_bytes(before.files[&a][..8].try_into().unwrap()));
            let entry_b = before.header.as_ref().unwrap().get_store_entry(package_b).unwrap();
            let entry_a = before.header.as_ref().unwrap().get_store_entry(package_a).unwrap();
            let imports_fixable = fixable && entry_a.imported_packages != entry_b.imported_packages;
            let mut o = options(false);
            o.pack.validate_on_replace = mode;
            crate::dialog::TEST_CONFIRM_TEXTS.lock().unwrap().clear();
            crate::win::TEST_MESSAGES.lock().unwrap().clear();
            // the second Warn round answers Yes
            static WARN_ROUND: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let warn_yes = mode == V::Warn && WARN_ROUND.fetch_add(1, std::sync::atomic::Ordering::SeqCst) % 2 == 1;
            if warn_yes {
                crate::dialog::TEST_CONFIRMS.lock().unwrap().push(true);
            }
            let r = pack::pack_files(&utoc.with_extension("ucas"), None, &src, std::slice::from_ref(&ib), pack::PK_PACK_SAVE_PATHS, &o);
            let after = snapshot(&utoc);
            let entry_after = after.header.as_ref().unwrap().get_store_entry(package_b).unwrap();
            let asked = crate::dialog::TEST_CONFIRM_TEXTS.lock().unwrap().clone();
            let shown = crate::win::TEST_MESSAGES.lock().unwrap().clone();
            match (mode, warn_yes) {
                (V::Error, _) => {
                    let Err(PackError::User(msg)) = r else { panic!("{}: Error mode packed", utoc.display()) };
                    assert!(msg.contains("Nothing was packed") && msg.contains(&ib), "{msg}");
                    assert_eq!(before.chunks, after.chunks);
                }
                (V::Warn, false) => {
                    assert!(matches!(r, Err(PackError::Aborted)), "{}", utoc.display());
                    assert_eq!(asked.len(), 1);
                    assert!(asked[0].contains("Pack them anyway?") && asked[0].contains(&ib), "{}", asked[0]);
                    assert_eq!(imports_fixable, asked[0].contains("ValidateOnReplace=Fix would correct"), "{}", asked[0]);
                    assert_eq!(before.chunks, after.chunks);
                }
                (V::Warn, true) => {
                    r.unwrap();
                    assert_eq!(entry_after, entry_b, "Warn never changes the entry");
                }
                (V::Fix, _) => {
                    r.unwrap();
                    assert!(asked.is_empty());
                    let report = shown.iter().find(|m| m.contains("ValidateOnReplace=Fix")).expect("Fix report");
                    assert!(report.contains("[not fixable] "), "{report}");
                    if imports_fixable {
                        fixable_seen = true;
                        assert!(report.contains("[fixed] "), "{report}");
                        assert_eq!(entry_after.imported_packages, entry_a.imported_packages, "imports rebuilt from the new file");
                        assert_eq!(entry_after.shader_map_hashes, entry_b.shader_map_hashes, "shader maps kept");
                    } else {
                        assert_eq!(entry_after, entry_b, "nothing to fix (before 5.3, or the same imports)");
                    }
                }
                (V::Off, _) => {
                    r.unwrap();
                    assert!(asked.is_empty() && !shown.iter().any(|m| m.contains("ValidateOnReplace")));
                    assert_eq!(entry_after, entry_b);
                }
            }
            assert!(!pack::undo_path(&utoc).exists());
        }
        eprintln!("validate modes: {} ({b} <- {a}, imports fixable: {fixable})", utoc.display());
    }
    assert!(fixable_seen, "no 5.3+ sample game with a fixable case");
}
