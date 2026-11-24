//! N2 (SPEC §8.9): legacy cooked packages (`.uasset`/`.umap` + `.uexp`) converted to Zen before
//! a copy is planned. The converted package replaces the input (written to a temporary file), its
//! `.uexp` leaves the list, and its bulk files (`.ubulk`/`.uptnl`/`.m.ubulk`, the same bytes in
//! both formats) are taken along from beside it when the copy doesn't name them. From there on the
//! copy is planned as raw Zen files, with the converted store entry attached to the input.

use crate::pack::{Game, Input, PackError};
use iostore::{ChunkId, ChunkKind, TocVersion};
use retoc::container_header::{EIoContainerHeaderVersion, StoreEntry};
use retoc::legacy_asset::{FLegacyPackageFileSummary, FSerializedAssetBundle};
use retoc::zen::FZenPackageHeader;
use retoc::zen_asset_conversion::{build_zen_asset, ConvertedZenAssetBundle, LegacyExportBundles};
use crate::dlog;
use retoc::zen::FPackageFileVersion;
use retoc::{EIoStoreTocVersion, FPackageId, UEPathBuf};
use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// What a converted input carries into planning.
#[derive(Debug, Clone)]
pub struct Converted {
    /// The store entry to-zen built (imports, and export counts/bundles before 5.3).
    pub entry: StoreEntry,
    /// The legacy `.uasset`/`.umap` it came from.
    pub legacy: PathBuf,
}

/// Temporary folder of the converted packages, removed when dropped.
pub struct TempDir(pub PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The inputs with legacy packages converted.
pub struct Prepared {
    pub inputs: Vec<Input>,
    pub converted: usize,
    /// Legacy packages that couldn't be converted (TC path, reason); left out of `inputs`, with
    /// their `.uexp`, so the copy's single refusal message can list them.
    pub refused: Vec<(String, String)>,
    _temp: Option<TempDir>,
}

const BULK_EXTS: [&str; 3] = ["ubulk", "uptnl", "m.ubulk"];

fn lower_ext(p: &str) -> String {
    let l = p.to_ascii_lowercase();
    if l.ends_with(".m.ubulk") {
        return "m.ubulk".into();
    }
    l.rsplit_once('.').map(|x| x.1.to_string()).unwrap_or_default()
}

fn stem(p: &str) -> &str {
    let l = p.to_ascii_lowercase();
    if l.ends_with(".m.ubulk") {
        return &p[..p.len() - 8];
    }
    p.rsplit_once('.').map(|x| x.0).unwrap_or(p)
}

/// Package file version for an unversioned legacy package of this game: the game's engine
/// versions (TOC + container-header version) that fit, and among those with different object
/// versions (5.6 vs 5.7/5.8) the one whose summary ends where the name map starts (UE writes the
/// name map right after the summary). 5.7 and 5.8 share the object version; 5.8's import layout is
/// recognised by the reader itself.
pub fn package_version_for(asset: &[u8], toc: EIoStoreTocVersion, header: EIoContainerHeaderVersion) -> Result<Option<FPackageFileVersion>, String> {
    let (info, names, _, _) = FLegacyPackageFileSummary::deserialize_summary_minimal_version_independent(&mut Cursor::new(asset)).map_err(|e| format!("not a legacy package: {e:#}"))?;
    if !info.is_unversioned {
        return Ok(None);
    }
    let mut versions: Vec<FPackageFileVersion> = Vec::new();
    for v in crate::legacy::candidates(toc, Some(header)).into_iter().rev() {
        let pv = v.package_file_version();
        if !versions.contains(&pv) {
            versions.push(pv);
        }
    }
    if versions.len() == 1 {
        return Ok(Some(versions[0]));
    }
    let fits: Vec<FPackageFileVersion> = versions
        .iter()
        .copied()
        .filter(|pv| {
            let mut c = Cursor::new(asset);
            FLegacyPackageFileSummary::deserialize(&mut c, Some(*pv)).is_ok() && c.position() == names.offset as u64
        })
        .collect();
    match fits.first() {
        Some(v) => Ok(Some(*v)),
        None if versions.is_empty() => Err(format!("no engine version known for this game (TOC {toc:?}, container header {header:?})")),
        None => Err("unversioned, and its header fits none of the engine versions this game can be".into()),
    }
}

/// Converts the legacy packages among `inputs`. `full_of`: the game path (`../../../…`) a TC path
/// goes to (needed for UE 4.27 package names); `toc`/`header`: the game's versions.
pub fn prepare(inputs: Vec<Input>, game: &Game, toc: TocVersion, header: Option<EIoContainerHeaderVersion>, full_of: &dyn Fn(&str) -> String) -> Result<Prepared, PackError> {
    let legacy: Vec<usize> = (0..inputs.len()).filter(|&i| matches!(lower_ext(&inputs[i].archive_path).as_str(), "uasset" | "umap") && crate::pack::is_legacy_package(&inputs[i].src)).collect();
    if legacy.is_empty() {
        return Ok(Prepared { inputs, converted: 0, refused: Vec::new(), _temp: None });
    }
    let mut refused: Vec<(String, String)> = Vec::new();
    let toc_enum = EIoStoreTocVersion::from_repr(toc.0);
    let (Some(toc_enum), Some(hv)) = (toc_enum, header) else {
        return Err(PackError::User(format!("Nothing was packed. Legacy packages can't be converted for this game: its TOC version ({}) or container-header version isn't known.", toc.0)));
    };
    let needs_fixup = hv <= EIoContainerHeaderVersion::Initial;
    let log = retoc::logging::Log::no_log();
    let temp = TempDir(std::env::temp_dir().join(format!("uetoc-pack-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0))));
    std::fs::create_dir_all(&temp.0)?;
    let mut drop_src: Vec<PathBuf> = Vec::new();
    let mut failed: Vec<usize> = Vec::new();
    let mut extra: Vec<Input> = Vec::new();
    let mut out: Vec<(usize, ConvertedZenAssetBundle)> = Vec::new();
    for &i in &legacy {
        let input = &inputs[i];
        let shown = input.archive_path.clone();
        let uexp = input.src.with_extension("uexp");
        if !uexp.is_file() {
            refused.push((shown, "legacy package without its .uexp (next to it or in this copy)".into()));
            failed.push(i);
            continue;
        }
        let asset = std::fs::read(&input.src)?;
        let fallback = match package_version_for(&asset, toc_enum, hv) {
            Ok(v) => v,
            Err(e) => {
                refused.push((shown, e));
                failed.push(i);
                drop_src.push(uexp);
                continue;
            }
        };
        let read_bulk = |ext: &str| -> Option<Vec<u8>> { std::fs::read(bulk_path(&input.src, ext)).ok() };
        let bundle = FSerializedAssetBundle {
            asset_file_buffer: asset,
            exports_file_buffer: std::fs::read(&uexp)?,
            bulk_data_buffer: read_bulk("ubulk"),
            optional_bulk_data_buffer: read_bulk("uptnl"),
            memory_mapped_bulk_data_buffer: read_bulk("m.ubulk"),
        };
        let path = UEPathBuf::from(full_of(&shown));
        let started = std::time::Instant::now();
        let converted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| build_zen_asset(bundle, &HashMap::new(), &path, fallback, hv, needs_fixup, None, None, &log)));
        match converted {
            Ok(Ok(c)) => {
                dlog!("  legacy {shown}: converted in {} ms ({} bytes, {} imports)", started.elapsed().as_millis(), c.package_data().len(), c.store_entry().imported_packages.len());
                drop_src.push(uexp.clone());
                // bulk files beside it that the copy doesn't name
                for ext in BULK_EXTS {
                    let p = bulk_path(&input.src, ext);
                    if p.is_file() && !inputs.iter().any(|x| crate::pack::same_path(&x.src, &p)) {
                        let len = std::fs::metadata(&p)?.len();
                        extra.push(Input { src: p, archive_path: format!("{}.{ext}", stem(&shown)), len, converted: None });
                    }
                }
                out.push((i, c));
            }
            Ok(Err(e)) => {
                refused.push((shown, format!("conversion failed: {e:#}")));
                failed.push(i);
                drop_src.push(uexp);
            }
            Err(_) => {
                refused.push((shown, "conversion failed (unexpected data)".into()));
                failed.push(i);
                drop_src.push(uexp);
            }
        }
    }
    // UE 4.27: arcs to the other packages' export bundles (converted together, else the game's)
    if needs_fixup {
        let lookup: HashMap<FPackageId, Arc<RwLock<ConvertedZenAssetBundle>>> = HashMap::new();
        let mut cache: HashMap<FPackageId, Option<LegacyExportBundles>> = HashMap::new();
        let mut headers = crate::pack::HeaderCache::default();
        // packages of this copy: their export bundles come from their own (new) headers
        for (_, c) in &out {
            if let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(c.package_data().to_vec()), Some(c.store_entry().clone()), toc_enum, hv, None) {
                cache.insert(c.package_id, Some(LegacyExportBundles::from_header(&h)));
            }
        }
        let imported: Vec<FPackageId> = out.iter().flat_map(|(_, c)| c.store_entry().imported_packages.clone()).collect();
        for pkg in imported {
            if !cache.contains_key(&pkg) {
                let v = game_export_bundles(game, toc, toc_enum, hv, pkg, &mut headers);
                cache.insert(pkg, v);
            }
        }
        for (i, c) in out.iter_mut() {
            let resolve = |pkg: FPackageId, index, command, name: Option<&str>| -> Option<i32> { cache.get(&pkg)?.as_ref()?.resolve(index, command, name) };
            let unresolved = c.fixup_legacy_external_arcs_with(&lookup, &resolve, &log).map_err(|e| PackError::User(format!("{}: {e:#}", inputs[*i].archive_path)))?;
            for u in unresolved {
                dlog!("  legacy {}: arc to {u} not resolved (package neither in the game nor in this copy)", inputs[*i].archive_path);
            }
        }
    }
    let mut result: Vec<Input> = Vec::new();
    let converted = out.len();
    let mut by_index: HashMap<usize, ConvertedZenAssetBundle> = out.into_iter().collect();
    for (i, input) in inputs.into_iter().enumerate() {
        if failed.contains(&i) || drop_src.iter().any(|d| crate::pack::same_path(d, &input.src)) {
            continue;
        }
        match by_index.remove(&i) {
            Some(c) => {
                let file = temp.0.join(format!("{i}.{}", lower_ext(&input.archive_path)));
                std::fs::write(&file, c.package_data())?;
                result.push(Input { src: file, archive_path: input.archive_path, len: c.package_data().len() as u64, converted: Some(Converted { entry: c.store_entry().clone(), legacy: input.src }) });
            }
            None => result.push(input),
        }
    }
    result.extend(extra);
    Ok(Prepared { inputs: result, converted, refused, _temp: Some(temp) })
}

fn bulk_path(asset: &Path, ext: &str) -> PathBuf {
    let s = asset.with_extension("").to_string_lossy().into_owned();
    PathBuf::from(format!("{s}.{ext}"))
}

/// Export bundles of a package the game already has (UE 4.27 arc resolution).
fn game_export_bundles(game: &Game, toc: TocVersion, toc_enum: EIoStoreTocVersion, hv: EIoContainerHeaderVersion, pkg: FPackageId, headers: &mut crate::pack::HeaderCache) -> Option<LegacyExportBundles> {
    let chunk = ChunkId::create(pkg.0, 0, ChunkKind::ExportBundleData, toc)?;
    let f = game.by_id.get(&chunk)?.first()?.clone();
    let entry = headers.get(&f.utoc).ok().flatten().and_then(|h| h.get_store_entry(pkg));
    let data = crate::pack::open_quiet(&f.utoc).ok()?.read_chunk(f.index).ok()?;
    let h = FZenPackageHeader::deserialize(&mut Cursor::new(data), entry, toc_enum, hv, None).ok()?;
    Some(LegacyExportBundles::from_header(&h))
}

/// The store entry for a converted package that replaces (or is copied over) a game package:
/// the converted one, with the game's shader-map hashes (shader libraries aren't converted, N2b).
pub fn merged_entry(converted: &StoreEntry, game: Option<&StoreEntry>) -> StoreEntry {
    let mut e = converted.clone();
    if e.shader_map_hashes.is_empty() {
        if let Some(g) = game {
            e.shader_map_hashes = g.shader_map_hashes.clone();
        }
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(lower_ext("A\\B.M.UBULK"), "m.ubulk");
        assert_eq!(stem("A\\B.m.ubulk"), "A\\B");
        assert_eq!(stem("A\\B.uasset"), "A\\B");
        assert_eq!(bulk_path(Path::new("/x/B.uasset"), "m.ubulk"), PathBuf::from("/x/B.m.ubulk"));
    }
}
