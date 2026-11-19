//! Plugin tests through the same paths TC uses (open → ReadHeader → ProcessFile).
//!
//! Sample tests need `IOSTORE_SAMPLES` (folder with the project's test containers) and,
//! for Oodle, `IOSTORE_OODLE`; without them they are skipped. AES keys: `key.txt` next to
//! the container (hex or base64), entered into the session cache like a dialog answer.

use super::*;
use crate::archive::ItemKind;
use crate::config::{MountPointMode, Options};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Tests share statics (session keys, dialog answers): run them one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn samples() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os("IOSTORE_SAMPLES")?);
    if let Some(o) = std::env::var_os("IOSTORE_OODLE") {
        iostore::set_oodle_search_paths([PathBuf::from(o)]);
    }
    Some(p)
}

fn utocs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut v: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    v.sort();
    for p in v {
        if p.is_dir() {
            utocs(&p, out);
        } else if p.extension().is_some_and(|e| e == "utoc") {
            out.push(p);
        }
    }
}

fn key_in(text: &str) -> Option<keys::AesKey> {
    text.split(|c: char| c.is_whitespace() || c == '"' || c == ',' || c == ':').find_map(|w| keys::AesKey::parse(w).ok())
}

/// Puts the folder's key.txt into the session cache (as if entered with "session only").
fn register_key(utoc: &Path) {
    if let Some(k) = std::fs::read_to_string(utoc.with_file_name("key.txt")).ok().and_then(|t| key_in(&t)) {
        let c = iostore::Container::open(utoc, profile::profile_for(utoc)).unwrap();
        keys::remember(utoc.parent().unwrap(), &c.header().key_guid.to_string(), &k, false).unwrap();
    }
}

fn kept(utoc: &Path) -> Option<Vec<u32>> {
    let text = std::fs::read_to_string(utoc.with_extension("trim.json")).ok()?;
    Some(text.split("\"index\":").skip(1).filter_map(|s| s.trim_start().split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()).collect())
}

fn listing_options() -> Options {
    Options { open_utoc_as_virtual_file: true, ..Options::default() }
}

fn all_items(arc: &mut Archive) -> Vec<read::HeaderOut> {
    let mut v = vec![];
    while let Some(h) = read::next_header(arc) {
        v.push(h);
    }
    v
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("uetoc-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn every_sample_lists_tests_and_extracts() {
    let _l = lock();
    let Some(root) = samples() else {
        eprintln!("IOSTORE_SAMPLES not set: skipped");
        return;
    };
    let mut list = vec![];
    utocs(&root, &mut list);
    assert!(!list.is_empty());
    let out = tmp("extract");
    for utoc in &list {
        register_key(utoc);
        let ucas = utoc.with_extension("ucas");
        let mut arc = read::open_archive(&ucas, &listing_options(), true).unwrap_or_else(|c| panic!("{}: {}", utoc.display(), code_name(c)));
        let n_chunks = arc.container.chunk_count() as usize;
        let items = all_items(&mut arc);
        let files = items.iter().filter(|h| h.attr & FILE_ATTR_DIRECTORY == 0 && !h.name.starts_with("$legacy\\")).count();
        assert_eq!(files, n_chunks, "{}: every chunk listed once", utoc.display());
        let keep = kept(utoc);
        // re-open and process every item: TEST for all, EXTRACT for a few
        let mut arc = read::open_archive(&ucas, &listing_options(), false).unwrap();
        let mut i = 0;
        let mut extracted = 0;
        while let Some(h) = read::next_header(&mut arc) {
            let kind = arc.current_item().unwrap().kind.clone();
            let real = match (&kind, &keep) {
                // the legacy view has its own test
                _ if h.name.starts_with("$legacy") => false,
                (ItemKind::Chunk(c), Some(k)) => k.contains(c),
                _ => true,
            };
            if !real {
                assert_eq!(read::process(&mut arc, PK_SKIP, None), 0);
                continue;
            }
            assert_eq!(read::process(&mut arc, PK_TEST, None), 0, "{}: test {}", utoc.display(), h.name);
            if matches!(kind, ItemKind::Chunk(_)) && extracted < 3 {
                let dest = out.join(format!("{i}")).join(h.name.replace('\\', "/"));
                assert_eq!(read::process(&mut arc, PK_EXTRACT, Some(dest.clone())), 0, "{}", h.name);
                assert_eq!(std::fs::metadata(&dest).unwrap().len(), h.unpack, "{}", h.name);
                extracted += 1;
            }
            i += 1;
        }
        eprintln!("{:<75} {files} files ok", utoc.strip_prefix(&root).unwrap().display());
    }
}

#[test]
fn utoc_opens_as_info_file() {
    let _l = lock();
    let Some(root) = samples() else { return };
    let utoc = root.join("vanilla/vanilla_ue/unecrypted/packed_as_versioned/ue5.8/pakchunk223-Windows.utoc");
    if !utoc.exists() {
        return;
    }
    let mut arc = read::open_archive(&utoc, &listing_options(), true).unwrap();
    let items = all_items(&mut arc);
    let id = arc.container.header().container_id.0;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].name, format!("{id}.txt"));
    let out = tmp("info").join("info.txt");
    assert_eq!(read::process(&mut arc, PK_EXTRACT, Some(out.clone())), 0);
    let text = std::fs::read_to_string(&out).unwrap();
    for section in ["[Container]", "[Header]", "[ContainerHeader]", "[Signatures]", "[Files]"] {
        assert!(text.contains(section), "{section}");
    }
    assert!(text.starts_with('\u{feff}'));
    // fixed-width: no tabs, values in one column, table rows after the "---" line
    assert!(!text.contains('\t'));
    let value_col = |key: &str| text.lines().find(|l| l.starts_with(key)).map(|l| l[key.len()..].find(|c: char| c != ' ').unwrap() + key.len()).unwrap();
    assert_eq!(value_col("Mount point"), value_col("Version"));
    assert_eq!(value_col("Folder"), value_col("Chunks without perfect hash"));
    assert!(text.lines().any(|l| l.starts_with("Mount point") && l.ends_with("../../../Moadra/Content/Mods/DebugInfo/")));
    assert!(text.lines().any(|l| l.starts_with("Version") && l.ends_with("5 SoftPackageReferencesOffset")));
    let mut table = text.lines().skip_while(|l| !l.trim_start().starts_with("#  Type"));
    let head = table.next().unwrap();
    let path_col = head.find("Path").unwrap();
    let rows: Vec<&str> = table.skip(1).take_while(|l| !l.is_empty()).collect();
    let chunk_rows = text.lines().skip_while(|l| !l.trim_start().starts_with("#  Chunk id")).skip(2).count();
    assert_eq!(chunk_rows, arc.container.chunk_count() as usize);
    assert!(text.lines().all(|l| l.chars().count() <= 120 || l.contains("/") || l.contains("\\")), "only lines with paths may be long");
    assert_eq!(rows.len(), arc.container.chunk_count() as usize);
    // every row's path starts in the Path column (all other columns are padded)
    assert!(rows.iter().all(|r| r.len() > path_col && r.as_bytes()[path_col - 1] == b' ' && r.as_bytes()[path_col] != b' '), "{rows:?}");
    assert_eq!(items[0].unpack as usize, std::fs::metadata(&out).unwrap().len() as usize);
    // OpenUtocAsVirtualFile=0: like the .ucas
    let o = Options { open_utoc_as_virtual_file: false, ..Options::default() };
    let mut arc = read::open_archive(&utoc, &o, true).unwrap();
    assert!(all_items(&mut arc).len() > 1);
}

#[test]
fn mount_point_views_and_chunks_folder() {
    let _l = lock();
    let Some(root) = samples() else { return };
    let ucas = root.join("vanilla/vanilla_ue/unecrypted/packed_as_versioned/ue5.8/pakchunk223-Windows.ucas");
    if !ucas.exists() {
        return;
    }
    let mut arc = read::open_archive(&ucas, &Options::default(), true).unwrap();
    let items = all_items(&mut arc);
    let names: Vec<&str> = items.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(&names[..4], ["Moadra", "Moadra\\Content", "Moadra\\Content\\Mods", "Moadra\\Content\\Mods\\DebugInfo"]);
    assert!(items[..4].iter().all(|h| h.attr == FILE_ATTR_DIRECTORY | config::ATTR_HIDDEN as c_int));
    assert!(names.iter().filter(|n| !n.starts_with("$chunks")).skip(4).all(|n| n.starts_with("Moadra\\Content\\Mods\\DebugInfo\\")));
    let chunks_dir = items.iter().find(|h| h.name == archive::CHUNKS_DIR).expect("$chunks folder");
    assert_eq!(chunks_dir.attr, FILE_ATTR_DIRECTORY | config::ATTR_HIDDEN as c_int);
    assert!(names.contains(&"$chunks\\b53fffed16aaad1300000006.ContainerHeader"), "{names:?}");
    let o = Options { mount_point: MountPointMode::Root, ..Options::default() };
    let mut arc = read::open_archive(&ucas, &o, true).unwrap();
    let items = all_items(&mut arc);
    assert!(items.iter().all(|h| !h.name.starts_with("Moadra")));
    assert!(items.iter().any(|h| h.name.ends_with(".uasset") && !h.name.contains('\\') || h.name.contains('\\')));
}

/// Copies a container (and optionally a global) into a temp tree.
fn stage(src_utoc: &Path, dir: &Path, global_dir: Option<&Path>) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    for ext in ["utoc", "ucas"] {
        std::fs::copy(src_utoc.with_extension(ext), dir.join(src_utoc.with_extension(ext).file_name().unwrap())).unwrap();
    }
    if let Some(g) = global_dir {
        std::fs::create_dir_all(g).unwrap();
        for f in ["global.utoc", "global.ucas"] {
            std::fs::copy(src_utoc.with_file_name(f), g.join(f)).unwrap();
        }
    }
    dir.join(src_utoc.with_extension("ucas").file_name().unwrap())
}

#[test]
fn global_lookup_rules() {
    let _l = lock();
    let Some(root) = samples() else { return };
    let src = root.join("vanilla/vanilla_ue/unecrypted/packed_as_versioned/ue5.3/pakchunk9-Windows.utoc");
    if !src.exists() {
        return;
    }
    let t = tmp("global");
    let o = Options::default();
    // global in Paks, container in Paks\~mods\Sub: found
    let ucas = stage(&src, &t.join("a/Content/Paks/~mods/Sub"), Some(&t.join("a/Content/Paks")));
    assert!(read::open_archive(&ucas, &o, true).is_ok());
    // global above Paks: not found
    let ucas = stage(&src, &t.join("b/Content/Paks/~mods"), Some(&t.join("b/Content")));
    assert_eq!(read::open_archive(&ucas, &o, true).err(), Some(E_EABORTED));
    // no Paks folder: limit applies
    let ucas = stage(&src, &t.join("c/x/y/z"), Some(&t.join("c")));
    assert!(read::open_archive(&ucas, &o, true).is_ok());
    let o1 = Options { global_lookup_limit: 1, ..Options::default() };
    assert_eq!(read::open_archive(&ucas, &o1, true).err(), Some(E_EABORTED));
    // a global without its .ucas isn't valid
    let ucas = stage(&src, &t.join("d/Paks"), Some(&t.join("d/Paks")));
    std::fs::remove_file(t.join("d/Paks/global.ucas")).unwrap();
    assert_eq!(read::open_archive(&ucas, &o, true).err(), Some(E_EABORTED));
    // .ucas without .utoc
    let lone = t.join("e/x.ucas");
    std::fs::create_dir_all(lone.parent().unwrap()).unwrap();
    std::fs::write(&lone, b"data").unwrap();
    assert_eq!(read::open_archive(&lone, &o, true).err(), Some(E_EABORTED));
    // partition name resolves to the base .utoc
    let p = stage(&src, &t.join("f/Paks"), Some(&t.join("f/Paks")));
    let s1 = p.with_file_name("pakchunk9-Windows_s1.ucas");
    std::fs::write(&s1, b"").unwrap();
    assert_eq!(read::utoc_for_ucas(&s1).unwrap(), p.with_extension("utoc"));
    assert!(read::looks_like_container(&p) && read::looks_like_container(&p.with_extension("utoc")));
    assert!(!read::looks_like_container(&lone));
}

#[test]
fn encrypted_key_prompt_flow() {
    let _l = lock();
    let Some(root) = samples() else { return };
    let src = root.join("vanilla/vanilla_ue/encrypted/4.27/pakchunk12-WindowsNoEditor.utoc");
    if !src.exists() {
        return;
    }
    let key = std::fs::read_to_string(src.with_file_name("key.txt")).ok().and_then(|t| key_in(&t)).unwrap();
    let t = tmp("keys");
    let ucas = stage(&src, &t.join("Paks"), Some(&t.join("Paks")));
    let o = Options::default();
    // a second container with the same key in the same folder
    let other = stage(&src.with_file_name("pakchunk2-WindowsNoEditor.utoc"), &t.join("Paks"), None);
    // no key, dialog cancelled: listing refused, info view opens with "(encrypted)"
    dialog::TEST_ANSWERS.lock().unwrap().clear();
    read::DECLINED.lock().unwrap().clear();
    let prompts = || dialog::TEST_PROMPTS.load(std::sync::atomic::Ordering::SeqCst);
    let before = prompts();
    assert_eq!(read::open_archive(&ucas, &o, true).err(), Some(E_EABORTED));
    // one cancel covers the folder (Find Files opens every container)
    assert_eq!(read::open_archive(&other, &o, true).err(), Some(E_EABORTED));
    assert_eq!(prompts() - before, 1);
    std::thread::sleep(std::time::Duration::from_millis(10));
    let mut arc = read::open_archive(&ucas.with_extension("utoc"), &o, true).unwrap();
    let out = t.join("info.txt");
    read::next_header(&mut arc).unwrap();
    assert_eq!(read::process(&mut arc, PK_EXTRACT, Some(out.clone())), 0);
    assert!(std::fs::read_to_string(&out).unwrap().contains("(encrypted)"));
    // after the 10 s grace: wrong key, then the right one (session only)
    read::DECLINED.lock().unwrap().clear();
    *dialog::TEST_ANSWERS.lock().unwrap() =
        vec![Some(("0x".to_string() + &"11".repeat(32), false)), Some((key.to_hex(), false))];
    let mut arc = read::open_archive(&ucas, &o, true).unwrap();
    assert_eq!(dialog::TEST_LAST_ERROR.lock().unwrap().as_deref(), Some("This key does not match this container."));
    assert!(all_items(&mut arc).len() >= 2);
    // now remembered for the session: no prompt
    assert!(dialog::TEST_ANSWERS.lock().unwrap().is_empty());
    assert!(read::open_archive(&ucas, &o, true).is_ok());
}

#[test]
fn damaged_data_fails_test_but_extracts() {
    let _l = lock();
    let Some(root) = samples() else { return };
    let src = root.join("vanilla/vanilla_ue/unecrypted/packed_as_versioned/ue5.3/pakchunk9-Windows.utoc");
    if !src.exists() {
        return;
    }
    let t = tmp("damaged");
    let ucas = stage(&src, &t.join("Paks"), Some(&t.join("Paks")));
    let arc = read::open_archive(&ucas, &Options::default(), false).unwrap();
    let (idx, big) = arc.container.chunks().max_by_key(|c| c.size).map(|c| (c.index, c.blocks.start)).unwrap();
    let off = arc.container.toc().blocks[big as usize].offset as usize + 100;
    drop(arc);
    let mut data = std::fs::read(&ucas).unwrap();
    data[off] ^= 0x5a;
    std::fs::write(&ucas, data).unwrap();
    let mut arc = read::open_archive(&ucas, &Options::default(), false).unwrap();
    let mut codes = vec![];
    while read::next_header(&mut arc).is_some() {
        if arc.current_item().unwrap().kind == ItemKind::Chunk(idx) {
            codes.push(read::process(&mut arc, PK_TEST, None));
            codes.push(read::process(&mut arc, PK_EXTRACT, Some(t.join("out.bin"))));
        }
    }
    assert_eq!(codes, [E_BAD_DATA, 0]);
}

/// `$legacy`: every package of every (untrimmed) sample converts; outputs match retoc's
/// own `to-legacy` when `RETOC_LEGACY_REF` points at its output for the same folder
/// (`<ref>/<sample folder relative to IOSTORE_SAMPLES>/...`).
#[test]
fn legacy_view_converts_samples() {
    let _l = lock();
    let Some(root) = samples() else { return };
    let reference = std::env::var_os("RETOC_LEGACY_REF").map(PathBuf::from);
    let mut list = vec![];
    utocs(&root, &mut list);
    let out = tmp("legacy");
    let (mut total, mut failed, mut compared, mut differ) = (0, 0, 0, 0);
    for utoc in &list {
        if kept(utoc).is_some() {
            continue; // trimmed: most data zeroed
        }
        register_key(utoc);
        legacy::reset();
        // versioned samples need no prompt; unversioned 5.6+ ones ask: answer the newest, this container only
        dialog::TEST_VERSION_ANSWERS.lock().unwrap().clear();
        dialog::TEST_VERSION_PROMPTS.lock().unwrap().clear();
        let ucas = utoc.with_extension("ucas");
        let mut arc = read::open_archive(&ucas, &Options::default(), false).unwrap();
        let folder = utoc.parent().unwrap().strip_prefix(&root).unwrap().to_path_buf();
        let (mut n, mut bad) = (0, 0);
        while let Some(h) = read::next_header(&mut arc) {
            if !h.name.starts_with("$legacy\\") || h.attr & FILE_ATTR_DIRECTORY != 0 {
                assert_eq!(read::process(&mut arc, PK_SKIP, None), 0);
                continue;
            }
            let rel = h.name["$legacy\\".len()..].replace('\\', "/");
            if std::env::var("LEGACY_FILTER").is_ok_and(|f| !rel.contains(&f)) {
                assert_eq!(read::process(&mut arc, PK_SKIP, None), 0);
                continue;
            }
            let dest = out.join(&folder).join(&rel);
            {
                let mut a = dialog::TEST_VERSION_ANSWERS.lock().unwrap();
                if a.is_empty() {
                    let pick = std::env::var("LEGACY_PICK").ok().and_then(|v| v.parse().ok()).unwrap_or(99);
                    a.push((pick, false, true));
                }
            }
            let code = read::process(&mut arc, PK_EXTRACT, Some(dest.clone()));
            n += 1;
            if code != 0 {
                bad += 1;
                eprintln!("  FAIL {} {}: {}", folder.display(), rel, code_name(code));
                continue;
            }
            if let Some(r) = &reference {
                let theirs = r.join(&folder).join(&rel);
                if let Ok(t) = std::fs::read(&theirs) {
                    compared += 1;
                    if t != std::fs::read(&dest).unwrap() {
                        differ += 1;
                        eprintln!("  DIFF {}", theirs.display());
                        if std::env::var_os("LEGACY_KEEP_DIFF").is_some() {
                            let _ = std::fs::copy(&dest, std::env::temp_dir().join(format!("diff-{differ}.bin")));
                        }
                    }
                }
                // big reference sets: don't keep a second copy
                let _ = std::fs::remove_file(&dest);
            }
        }
        if utoc.to_string_lossy().contains("packed_as_versioned") {
            assert!(dialog::TEST_VERSION_PROMPTS.lock().unwrap().is_empty(), "{}: versioned packages need no engine version", utoc.display());
        }
        total += n;
        failed += bad;
        eprintln!("{:<75} {n} legacy files, {bad} failed", utoc.strip_prefix(&root).unwrap().display());
    }
    eprintln!("legacy: {total} files, {failed} failed, {compared} compared with retoc, {differ} differ");
    assert_eq!(failed, 0);
    assert_eq!(differ, 0);
}

/// Engine-version prompt rules (SPEC §6a) on an unversioned UE 5.6 container
/// (`IOSTORE_UNVERSIONED_56` = its `.utoc`, e.g. The Isle server's pakchunk2; key in key.txt).
#[test]
fn legacy_version_prompt() {
    let _l = lock();
    let Some(utoc) = std::env::var_os("IOSTORE_UNVERSIONED_56").map(PathBuf::from) else { return };
    if let Some(o) = std::env::var_os("IOSTORE_OODLE") {
        iostore::set_oodle_search_paths([PathBuf::from(o)]);
    }
    register_key(&utoc);
    legacy::reset();
    let ucas = utoc.with_extension("ucas");
    let prompts = || dialog::TEST_VERSION_PROMPTS.lock().unwrap().clone();
    let answer = |a: (usize, bool, bool)| dialog::TEST_VERSION_ANSWERS.lock().unwrap().push(a);
    let out = tmp("legacyver");
    // extracts the first `n` package files of a fresh handle; returns the codes
    let run = |o: &Options, n: usize| -> Vec<c_int> {
        let mut arc = read::open_archive(&ucas, o, false).unwrap();
        let mut codes = vec![];
        while let Some(h) = read::next_header(&mut arc) {
            if codes.len() < n && h.name.starts_with("$legacy\\") && h.name.ends_with(".uasset") {
                codes.push(read::process(&mut arc, PK_EXTRACT, Some(out.join(h.name.replace('\\', "/")))));
            } else {
                read::process(&mut arc, PK_SKIP, None);
            }
        }
        codes
    };
    dialog::TEST_VERSION_PROMPTS.lock().unwrap().clear();
    dialog::TEST_VERSION_ANSWERS.lock().unwrap().clear();
    let auto = Options::default();
    // Auto, ambiguous header: one prompt per handle, versions that fit, newest preselected
    answer((0, false, false));
    assert_eq!(run(&auto, 3), [0, 0, 0]);
    assert_eq!(prompts(), [(vec!["UE 5.6".to_string(), "UE 5.7 / 5.8".to_string()], 1)]);
    // no "don't ask again": the next handle asks again; with it, not any more
    answer((1, false, true));
    assert_eq!(run(&auto, 2), [0, 0]);
    assert_eq!(prompts().len(), 2);
    assert_eq!(run(&auto, 2), [0, 0]);
    assert_eq!(prompts().len(), 2);
    // opening another container ends the pick
    let global = utoc.with_file_name("global.utoc");
    let _ = read::open_archive(&global, &auto, false);
    answer((0, false, false));
    assert_eq!(run(&auto, 1), [0]);
    assert_eq!(prompts().len(), 3);
    // cancel: aborts this handle's legacy files
    assert_eq!(run(&auto, 2), [E_EABORTED, E_EABORTED]);
    assert_eq!(prompts().len(), 4);
    // a fixed ini value that fits: no prompt; one that doesn't fit (4.27): Auto, so a prompt
    // with the ini value not in the list (newest preselected)
    let fixed = Options { legacy_engine_version: Some(retoc::version::EngineVersion::UE5_7), ..Options::default() };
    assert_eq!(run(&fixed, 2), [0, 0]);
    assert_eq!(prompts().len(), 4);
    let wrong = Options { legacy_engine_version: Some(retoc::version::EngineVersion::UE4_27), ..Options::default() };
    answer((0, false, false));
    assert_eq!(run(&wrong, 1), [0]);
    assert_eq!(prompts().len(), 5);
    // 5.6 ini value preselects 5.6 when asking is needed: covered by `selected` = ini position
    let five_six = Options { legacy_engine_version: Some(retoc::version::EngineVersion::UE5_6), ..Options::default() };
    assert_eq!(run(&five_six, 1), [0]);
    assert_eq!(prompts().len(), 5, "a fitting ini value never asks");
}
