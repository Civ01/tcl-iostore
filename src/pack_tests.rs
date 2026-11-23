//! Packing tests (SPEC §8) on copies of whole sample game folders (`IOSTORE_SAMPLES`).
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

        // into B (a mount-point change asks first, as tcl-uepak: answered Yes)
        crate::dialog::TEST_CONFIRM_TEXTS.lock().unwrap().clear();
        crate::dialog::TEST_CONFIRMS.lock().unwrap().push(true);
        let st = pack::pack_files(&b.with_extension("ucas"), None, &src, std::slice::from_ref(&shown), pack::PK_PACK_SAVE_PATHS, &options(false)).unwrap_or_else(|e| panic!("{}: {e}", b.display()));
        let asked = crate::dialog::TEST_CONFIRM_TEXTS.lock().unwrap().clone();
        if asked.is_empty() {
            crate::dialog::TEST_CONFIRMS.lock().unwrap().clear();
        } else {
            assert!(asked[0].contains("outside the container's mount point") && !asked[0].contains("New files ("), "{}", asked[0]);
        }
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
            assert!(msg.contains("companion .pak") && msg.contains("Legacy cooked"), "{msg}");
        }
        other => panic!("expected a refusal, got {:?}", other.err()),
    }
    // a new file that isn't a Zen package of this game: refused (N1b), nothing asked
    crate::dialog::TEST_CONFIRM_TEXTS.lock().unwrap().clear();
    match pack::pack_files(&utoc.with_extension("ucas"), Some("Game\\Content"), &src, &["New.uasset".to_string()], pack::PK_PACK_SAVE_PATHS, &options(false)) {
        Err(PackError::User(msg)) => assert!(msg.contains("can't be added") && (msg.contains("not a Zen package") || msg.contains("UE 5.3+")), "{msg}"),
        other => panic!("expected a refusal, got {:?}", other.err()),
    }
    assert!(crate::dialog::TEST_CONFIRM_TEXTS.lock().unwrap().is_empty());
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
/// mode does what SPEC §8.5 says, in every sample game.
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

/// A package of `utoc` (TC display path, game key, chunk id), preferring one with imports and,
/// when `with_bulk`, one with a `.ubulk` next to it.
fn pick_package(utoc: &Path, with_bulk: bool) -> Option<(String, String, [u8; 12], Option<String>)> {
    let s = snapshot(utoc);
    let mut c = Container::open(utoc, Arc::new(Vanilla)).unwrap();
    let idx = c.directory_index().unwrap().clone();
    let prefix = Mount::parse(&idx.mount_point).display_prefix();
    let shown_of = |p: &str| if prefix.is_empty() { p.replace('/', "\\") } else { format!("{prefix}\\{}", p.replace('/', "\\")) };
    let mut best = None;
    for f in &idx.files {
        let key = pack::game_key(&idx.mount_point, &f.path);
        let id = s.chunks.keys().find(|k| **k == s.files[&key]).copied().unwrap();
        if !key.ends_with(".uasset") || iostore::ChunkId(id).kind(s.version) != ChunkKind::ExportBundleData {
            continue;
        }
        let bulk_rel = f.path.replace(".uasset", ".ubulk");
        let bulk = idx.files.iter().any(|g| g.path == bulk_rel).then(|| shown_of(&bulk_rel));
        if with_bulk && bulk.is_none() {
            continue;
        }
        let imports = s.header.as_ref().and_then(|h| h.get_store_entry(retoc::FPackageId(u64::from_le_bytes(id[..8].try_into().unwrap())))).map(|e| e.imported_packages.len()).unwrap_or(0);
        let cand = (shown_of(&f.path), key, id, bulk);
        if imports > 0 || with_bulk {
            return Some(cand);
        }
        best.get_or_insert(cand);
    }
    let _ = c.read_chunk(0);
    best
}

fn confirm_answers(a: &[bool]) {
    crate::dialog::TEST_CONFIRM_TEXTS.lock().unwrap().clear();
    let mut q = crate::dialog::TEST_CONFIRMS.lock().unwrap();
    q.clear();
    q.extend_from_slice(a);
}

fn asked() -> Vec<String> {
    crate::dialog::TEST_CONFIRM_TEXTS.lock().unwrap().clone()
}

fn entry_of(s: &Snapshot, id: [u8; 12]) -> Option<retoc::container_header::StoreEntry> {
    s.header.as_ref()?.get_store_entry(retoc::FPackageId(u64::from_le_bytes(id[..8].try_into().unwrap())))
}

/// N1b: a package the game doesn't have (deleted from a copy of the game first) is added back
/// with the chunk id of its internal name and a store entry rebuilt from its own imports; the
/// question comes once; No writes nothing; before UE 5.3 it's refused.
#[test]
fn new_package_is_added_with_a_derived_entry() {
    use crate::config::ValidateOnAdd as A;
    let _l = lock();
    if games().is_empty() {
        return;
    }
    let mut seen_new = false;
    let mut seen_old = false;
    for (n, g) in games().iter().enumerate() {
        let probe = copy_game(g, &format!("new{n}"));
        let Some(utoc) = targets(&probe).into_iter().find(|t| pick_package(t, false).is_some()) else { continue };
        let name = utoc.file_name().unwrap().to_owned();
        let header_version = snapshot(&utoc).header.as_ref().unwrap().version;
        let supported = crate::validate::names_imports(header_version);
        for (round, answer) in [(0, false), (1, true)] {
            let copy = copy_game(g, &format!("new{n}-{round}"));
            let utoc = copy.join(&name);
            let (shown, key, id, _) = pick_package(&utoc, false).unwrap();
            let original = snapshot(&utoc);
            let src = copy.join("src");
            let items = extract_named(&utoc, &src);
            assert!(items.contains(&shown));
            // the game no longer has it
            assert_eq!(pack::delete_files(&utoc.with_extension("ucas"), std::slice::from_ref(&shown), &options(false)).unwrap().removed, 1);
            let before = snapshot(&utoc);
            confirm_answers(&[answer]);
            let r = pack::pack_files(&utoc.with_extension("ucas"), None, &src, std::slice::from_ref(&shown), pack::PK_PACK_SAVE_PATHS, &options(false));
            let after = snapshot(&utoc);
            if !supported {
                let Err(PackError::User(msg)) = r else { panic!("{}: added before 5.3", utoc.display()) };
                assert!(msg.contains("UE 5.3+"), "{msg}");
                assert!(asked().is_empty());
                assert_eq!(before.chunks, after.chunks);
                seen_old = true;
                break;
            }
            let q = asked();
            assert_eq!(q.len(), 1, "{q:?}");
            assert!(q[0].contains("Adding new files to an existing container may lead to unpredictable results.") && q[0].contains(&shown.replace('\\', "/")), "{}", q[0]);
            if !answer {
                assert!(matches!(r, Err(PackError::Aborted)));
                assert_eq!(before.chunks, after.chunks);
                assert_eq!(before.header, after.header);
                continue;
            }
            let st = r.unwrap_or_else(|e| panic!("{}: {e}", utoc.display()));
            assert_eq!((st.replaced, st.added), (0, 1));
            // same chunk id, data, path as the cook made them
            assert_eq!(after.files.get(&key), Some(&id), "chunk id from the internal name");
            assert_eq!(after.chunks[&id], original.chunks[&id]);
            assert_eq!(after.files, original.files);
            // the entry rebuilt from the package's own imports is the cook's
            let (orig, now) = (entry_of(&original, id).unwrap(), entry_of(&after, id).unwrap());
            assert_eq!(now.imported_packages, orig.imported_packages, "{}", utoc.display());
            assert!(now.shader_map_hashes.is_empty());
            seen_new = true;
        }
        eprintln!("new package: {} (5.3+: {supported})", utoc.display());
        let _ = A::Warn;
    }
    assert!(seen_new, "no 5.3+ sample game");
    assert!(seen_old, "no pre-5.3 sample game");
}

/// N1b: ValidateOnAdd on a new package dropped in the wrong folder (same content root), every mode.
#[test]
fn validate_on_add_modes() {
    use crate::config::ValidateOnAdd as A;
    let _l = lock();
    let Some((g, name)) = games().into_iter().find_map(|g| {
        let t = targets(&g).into_iter().find(|t| pick_package(t, false).is_some() && snapshot(t).header.as_ref().is_some_and(|h| crate::validate::names_imports(h.version)))?;
        Some((g, t.file_name().unwrap().to_owned()))
    }) else {
        return eprintln!("no 5.3+ sample game: skipped");
    };
    for mode in [A::Warn, A::Fix, A::Error, A::Off] {
        let copy = copy_game(&g, &format!("vadd-{mode:?}"));
        let utoc = copy.join(&name);
        let (shown, key, id, _) = pick_package(&utoc, false).unwrap();
        let src = copy.join("src");
        extract_named(&utoc, &src);
        pack::delete_files(&utoc.with_extension("ucas"), std::slice::from_ref(&shown), &options(false)).unwrap();
        // the same file one folder deeper: its path no longer gives its name
        let (dir, file) = shown.rsplit_once('\\').unwrap();
        let wrong = format!("{dir}\\WrongFolder\\{file}");
        let wrong_path = src.join(wrong.replace('\\', "/"));
        std::fs::create_dir_all(wrong_path.parent().unwrap()).unwrap();
        std::fs::copy(src.join(shown.replace('\\', "/")), &wrong_path).unwrap();
        let mut o = options(false);
        o.pack.validate_on_add = mode;
        confirm_answers(&[true]);
        let r = pack::pack_files(&utoc.with_extension("ucas"), None, &src, std::slice::from_ref(&wrong), pack::PK_PACK_SAVE_PATHS, &o);
        let after = snapshot(&utoc);
        let q = asked();
        let at = |k: &str| after.files.iter().find(|(p, _)| p.as_str() == k).map(|x| *x.1);
        let wrong_key = key.replacen(&file.to_lowercase(), &format!("wrongfolder/{}", file.to_lowercase()), 1);
        match mode {
            A::Error => {
                let Err(PackError::User(msg)) = r else { panic!("Error mode packed") };
                assert!(msg.contains("ValidateOnAdd=Error") && msg.contains("belongs at"), "{msg}");
                assert!(q.is_empty() && at(&key).is_none() && at(&wrong_key).is_none());
            }
            A::Warn => {
                r.unwrap();
                assert!(q[0].contains("[fixable]") && q[0].contains("belongs at"), "{}", q[0]);
                assert_eq!(at(&wrong_key), Some(id), "Warn packs where dropped");
            }
            A::Fix => {
                r.unwrap();
                assert!(q[0].contains("Moved to the path their name gives") && !q[0].contains("[fixable]"), "{}", q[0]);
                assert_eq!(at(&key), Some(id), "Fix moves it back");
                assert!(at(&wrong_key).is_none());
            }
            A::Off => {
                r.unwrap();
                assert!(!q[0].contains("belongs at") && !q[0].contains("ValidateOnAdd"), "{}", q[0]);
                assert_eq!(at(&wrong_key), Some(id));
            }
        }
    }
}

/// N1b: a new package from another container (outside this one's mount point) asks once with
/// the uepak-style mount-point block; a new container takes new files too (with the question).
#[test]
fn new_files_mount_point_and_new_container() {
    let _l = lock();
    let Some(g) = games().into_iter().find(|g| {
        let t = targets(g);
        t.len() >= 2 && snapshot(&t[0]).header.as_ref().is_some_and(|h| crate::validate::names_imports(h.version))
    }) else {
        return eprintln!("no 5.3+ game with two containers: skipped");
    };
    let copy = copy_game(&g, "newmount");
    let t = targets(&copy);
    // a container whose mount point the other's package doesn't fit
    let pair = t.iter().flat_map(|a| t.iter().map(move |b| (a.clone(), b.clone()))).find(|(a, b)| {
        if a == b {
            return false;
        }
        let (Ok(mut ca), Ok(mut cb)) = (Container::open(a, Arc::new(Vanilla)), Container::open(b, Arc::new(Vanilla))) else { return false };
        let (ma, mb) = (ca.directory_index().unwrap().mount_point.clone(), cb.directory_index().unwrap().mount_point.clone());
        pick_package(a, false).is_some() && !ma.to_lowercase().starts_with(&mb.to_lowercase())
    });
    let Some((a, b)) = pair else { return eprintln!("no container pair with different mount points: skipped") };
    let (shown, key, id, _) = pick_package(&a, false).unwrap();
    let src = copy.join("src");
    extract_named(&a, &src);
    pack::delete_files(&a.with_extension("ucas"), std::slice::from_ref(&shown), &options(false)).unwrap();
    let before_b = snapshot(&b);
    confirm_answers(&[true]);
    let st = pack::pack_files(&b.with_extension("ucas"), None, &src, std::slice::from_ref(&shown), pack::PK_PACK_SAVE_PATHS, &options(false)).unwrap();
    assert_eq!(st.added, 1);
    let q = asked();
    assert_eq!(q.len(), 1);
    assert!(q[0].contains("New files (1)") && q[0].contains("outside the container's mount point") && q[0].contains("Where the game finds them doesn't change"), "{}", q[0]);
    let after_b = snapshot(&b);
    assert_eq!(after_b.files.get(&key), Some(&id));
    for (k, v) in &before_b.files {
        assert_eq!(after_b.files.get(k), Some(v), "existing files keep their game paths: {k}");
    }
    // a new container with a new file: asked too
    pack::delete_files(&b.with_extension("ucas"), std::slice::from_ref(&shown), &options(false)).unwrap();
    let newc = copy.join("zzz_new_P.utoc");
    confirm_answers(&[true]);
    let st = pack::pack_files(&newc.with_extension("ucas"), None, &src, std::slice::from_ref(&shown), pack::PK_PACK_SAVE_PATHS, &options(false)).unwrap();
    assert!(st.new_container && st.added == 1);
    let q = asked();
    assert!(q.len() == 1 && q[0].contains("This new container gets files the game doesn't have"), "{q:?}");
    let sn = snapshot(&newc);
    assert_eq!(sn.files.get(&key), Some(&id));
    assert!(entry_of(&sn, id).is_some());
}

/// N1b: bulk data travels with its package (id = package id + bulk kind, index 0); alone
/// (package not in the game) it's refused. No 5.3+ sample has bulk files, so the `.ubulk` is
/// made up next to a real package: this checks ids, paths and refusals, not bulk content.
#[test]
fn bulk_files_follow_their_package() {
    let _l = lock();
    let Some((g, name)) = games().into_iter().find_map(|g| {
        let t = targets(&g).into_iter().find(|t| pick_package(t, false).is_some() && snapshot(t).header.as_ref().is_some_and(|h| crate::validate::names_imports(h.version)))?;
        Some((g, t.file_name().unwrap().to_owned()))
    }) else {
        return eprintln!("no 5.3+ sample game: skipped");
    };
    let copy = copy_game(&g, "bulk");
    let utoc = copy.join(&name);
    let (shown, key, id, _) = pick_package(&utoc, false).unwrap();
    let src = copy.join("src");
    extract_named(&utoc, &src);
    pack::delete_files(&utoc.with_extension("ucas"), std::slice::from_ref(&shown), &options(false)).unwrap();
    let bulk = shown.replace(".uasset", ".ubulk");
    let mmap = shown.replace(".uasset", ".m.ubulk");
    std::fs::write(src.join(bulk.replace('\\', "/")), vec![0x5Au8; 3000]).unwrap();
    std::fs::write(src.join(mmap.replace('\\', "/")), vec![0xA5u8; 1000]).unwrap();
    let version = snapshot(&utoc).version;
    // alone: refused, nothing asked
    confirm_answers(&[true]);
    match pack::pack_files(&utoc.with_extension("ucas"), None, &src, std::slice::from_ref(&bulk), pack::PK_PACK_SAVE_PATHS, &options(false)) {
        Err(PackError::User(msg)) => assert!(msg.contains("bulk data without its package"), "{msg}"),
        other => panic!("expected a refusal, got {:?}", other.err()),
    }
    assert!(asked().is_empty());
    // with its package: one question, three files, ids derived from the package's
    confirm_answers(&[true]);
    let all = [shown.clone(), bulk.clone(), mmap.clone()];
    let st = pack::pack_files(&utoc.with_extension("ucas"), None, &src, &all, pack::PK_PACK_SAVE_PATHS, &options(false)).unwrap();
    assert_eq!(st.added, 3);
    assert_eq!(asked().len(), 1);
    let after = snapshot(&utoc);
    let pkg = u64::from_le_bytes(id[..8].try_into().unwrap());
    assert_eq!(after.files.get(&key), Some(&id));
    let want = |kind| iostore::ChunkId::create(pkg, 0, kind, version).unwrap().0;
    assert_eq!(after.files.get(&key.replace(".uasset", ".ubulk")), Some(&want(ChunkKind::BulkData)));
    assert_eq!(after.files.get(&key.replace(".uasset", ".m.ubulk")), Some(&want(ChunkKind::MemoryMappedBulkData)));
    assert_eq!(after.chunks[&want(ChunkKind::BulkData)], vec![0x5Au8; 3000]);
    // the memory-mapped one is flagged so (16 KiB aligned by the writer)
    let c = Container::open(&utoc, Arc::new(Vanilla)).unwrap();
    let mm = c.chunks().find(|i| i.id.0 == want(ChunkKind::MemoryMappedBulkData)).unwrap();
    assert!(mm.memory_mapped());
    // only the package has a store entry
    assert!(entry_of(&after, id).is_some());
}

fn mount_answers(a: &[Option<&str>]) {
    crate::dialog::TEST_MOUNT_PROMPTS.lock().unwrap().clear();
    pack::TEST_SAVED_MOUNTS.lock().unwrap().clear();
    let mut q = crate::dialog::TEST_MOUNT_ANSWERS.lock().unwrap();
    q.clear();
    q.extend(a.iter().map(|x| x.map(String::from)));
}

fn mount_prompts() -> Vec<(Vec<String>, String, String)> {
    crate::dialog::TEST_MOUNT_PROMPTS.lock().unwrap().clone()
}

fn mount_of(utoc: &Path) -> String {
    Container::open(utoc, Arc::new(Vanilla)).unwrap().directory_index().unwrap().mount_point.clone()
}

/// SPEC §8.3 rule B, without a game.
#[test]
fn new_container_paths_from_the_mount_point() {
    let m = Some("../../../Moadra/Content/Mods/SML/");
    let none = |_: &str| false;
    // a file alone: mount point + its path
    assert_eq!(pack::new_container_full("BP_SML.uasset", m, none), "../../../Moadra/Content/Mods/SML/BP_SML.uasset");
    // the whole tree: already starts with the mount point's folders, taken as it is
    assert_eq!(pack::new_container_full("Moadra/Content/Mods/SML/BP_SML.uasset", m, none), "../../../Moadra/Content/Mods/SML/BP_SML.uasset");
    assert_eq!(pack::new_container_full("moadra/content/mods/sml/X.uasset", m, none), "../../../moadra/content/mods/sml/X.uasset");
    // only a part of the folders: prefixed (and then simply not the game's path)
    assert_eq!(pack::new_container_full("Mods/SML/BP_SML.uasset", m, none), "../../../Moadra/Content/Mods/SML/Mods/SML/BP_SML.uasset");
    // a folder that merely starts with the same letters isn't the mount point's folder
    assert_eq!(pack::new_container_full("Moadra/Content/Mods/SMLX/a.uasset", m, none), "../../../Moadra/Content/Mods/SML/Moadra/Content/Mods/SMLX/a.uasset");
    // both readings possible: the game decides, else the full-path reading
    let doubled = "../../../Moadra/Content/Mods/SML/Moadra/Content/Mods/SML/a.uasset";
    assert_eq!(pack::new_container_full("Moadra/Content/Mods/SML/a.uasset", m, |f| f == doubled), doubled);
    assert_eq!(pack::new_container_full("Moadra/Content/Mods/SML/a.uasset", m, |_| true), "../../../Moadra/Content/Mods/SML/a.uasset");
    // Auto and the root mount point
    assert_eq!(pack::new_container_full("Moadra/a.uasset", None, none), "../../../Moadra/a.uasset");
    assert_eq!(pack::new_container_full("Moadra/a.uasset", Some("../../../"), none), "../../../Moadra/a.uasset");
}

/// SPEC §8.3: a game file dropped alone or with its whole path into a new container, with the
/// mount point asked (preset = remembered, else the game's folder of the package), Auto, a wrong
/// mount point (refused, named in the message), Cancel, AskMountPoint=0 and the history.
#[test]
fn new_container_mount_point_dialog() {
    let _l = lock();
    let mut tested = 0;
    for g in games() {
        let Some(t0) = targets(&g).into_iter().find(|t| pick_package(t, false).is_some()) else { continue };
        let named = snapshot(&t0).header.as_ref().is_some_and(|h| crate::validate::names_imports(h.version));
        let copy = copy_game(&g, "mountdlg");
        let utoc = copy.join(t0.file_name().unwrap());
        let (shown, key, id, _) = pick_package(&utoc, false).unwrap();
        let Some((folder, base)) = shown.rsplit_once('\\') else { continue };
        let folder_mount = format!("../../../{}/", folder.replace('\\', "/"));
        let tree = copy.join("tree");
        extract_named(&utoc, &tree);
        let alone = copy.join("alone");
        std::fs::create_dir_all(&alone).unwrap();
        std::fs::copy(tree.join(shown.replace('\\', "/")), alone.join(base)).unwrap();
        let pack_new = |name: &str, src: &Path, item: &str, o: &Options| pack::pack_files(&copy.join(name).with_extension("ucas"), None, src, &[item.to_string()], pack::PK_PACK_SAVE_PATHS, o);

        // 1. alone, nothing remembered: preset = the game's folder of the package (5.3+: the name
        //    is read; older: no suggestion, preset Auto, which can't place the file -> refused)
        mount_answers(&[]);
        confirm_answers(&[]);
        let r = pack_new("zzz_m1_P", &alone, base, &options(false));
        let p = mount_prompts();
        assert_eq!(p.len(), 1, "{g:?}");
        assert!(p[0].0.iter().any(|c| c == "Auto"));
        if named {
            let st = r.unwrap_or_else(|e| panic!("{g:?}: {e:?}"));
            assert_eq!(p[0].1, folder_mount, "{g:?}");
            assert!(p[0].2.contains("The game has 1 of 1"), "{}", p[0].2);
            assert!(st.new_container && st.added == 1, "{st:?}");
            let sn = snapshot(&copy.join("zzz_m1_P.utoc"));
            assert_eq!(sn.files.get(&key), Some(&id), "{g:?}");
            assert_eq!(mount_of(&copy.join("zzz_m1_P.utoc")), folder_mount);
            assert_eq!(*pack::TEST_SAVED_MOUNTS.lock().unwrap(), [folder_mount.clone()]);
            assert!(asked().is_empty(), "a game file asks nothing more: {:?}", asked());
        } else {
            assert_eq!(p[0].1, "Auto");
            assert!(r.is_err() && !copy.join("zzz_m1_P.utoc").exists());
        }

        // 2. the whole tree with the same (remembered) mount point: the same container
        let mut o = options(false);
        o.pack.recent_mount_points = vec![folder_mount.clone(), "Auto".into()];
        mount_answers(&[]);
        let st = pack_new("zzz_m2_P", &tree, &shown, &o).unwrap_or_else(|e| panic!("{g:?}: {e:?}"));
        assert!(st.added == 1);
        let p = mount_prompts();
        assert_eq!(p[0].1, folder_mount);
        assert!(p[0].2.contains("The game has 1 of 1"), "{}", p[0].2);
        assert_eq!(snapshot(&copy.join("zzz_m2_P.utoc")).files.get(&key), Some(&id));
        assert_eq!(mount_of(&copy.join("zzz_m2_P.utoc")), folder_mount);

        // 3. the tree with Auto picked from the list: the history is reordered
        mount_answers(&[Some("Auto")]);
        let st = pack_new("zzz_m3_P", &tree, &shown, &o).unwrap();
        assert!(st.added == 1);
        assert_eq!(snapshot(&copy.join("zzz_m3_P.utoc")).files.get(&key), Some(&id));
        assert_eq!(*pack::TEST_SAVED_MOUNTS.lock().unwrap(), ["Auto".to_string(), folder_mount.clone()]);

        // 4. a mount point the file doesn't belong under: refused, the message names it
        mount_answers(&[Some(r"..\..\..\Nowhere")]);
        let e = pack_new("zzz_m4_P", &alone, base, &o).unwrap_err();
        let PackError::User(msg) = e else { panic!("{e:?}") };
        assert!(msg.contains("Mount point for the copied files: ../../../Nowhere/"), "{msg}");
        assert!(!copy.join("zzz_m4_P.utoc").exists());

        // 5. a bad text: the dialog's preview refuses it (OK stays disabled)
        mount_answers(&[Some("C:/Game"), None]);
        let (_, _, preview) = {
            let _ = pack_new("zzz_m5_P", &alone, base, &o);
            mount_prompts().remove(0)
        };
        assert!(preview.starts_with("ERR") && preview.contains("not a mount point"), "{preview}");

        // 6. Cancel: nothing written, nothing remembered
        mount_answers(&[None]);
        assert!(matches!(pack_new("zzz_m6_P", &tree, &shown, &o), Err(PackError::Aborted)));
        assert!(!copy.join("zzz_m6_P.utoc").exists());
        assert!(pack::TEST_SAVED_MOUNTS.lock().unwrap().is_empty());

        // 7. AskMountPoint=0: the last one, no dialog; history size 1 keeps only it
        let mut o7 = o.clone();
        o7.pack.ask_mount_point = false;
        o7.pack.mount_point_history = 1;
        mount_answers(&[]);
        let st = pack_new("zzz_m7_P", &alone, base, &o7);
        assert!(mount_prompts().is_empty());
        assert_eq!(*pack::TEST_SAVED_MOUNTS.lock().unwrap(), [folder_mount.clone()]);
        let st = st.unwrap();
        assert!(st.added == 1);
        assert_eq!(snapshot(&copy.join("zzz_m7_P.utoc")).files.get(&key), Some(&id));
        tested += 1;
        let _ = std::fs::remove_dir_all(&copy);
    }
    eprintln!("mount point dialog: {tested} games");
}
