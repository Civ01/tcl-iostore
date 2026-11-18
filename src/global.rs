//! Finding the container's global container (SPEC §3.2).
//!
//! `global.utoc` + `global.ucas` in the container's folder, then each parent folder, up
//! to `GlobalParentLookupLimit` parent levels, never above the nearest folder named
//! `Paks` (which is still searched).

use std::path::{Path, PathBuf};

use iostore::{Container, TocVersion};

#[derive(Debug, Clone)]
pub struct GlobalInfo {
    pub utoc: PathBuf,
    pub version: TocVersion,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StopReason {
    /// The nearest `Paks` folder was searched; the search doesn't go above it.
    PaksBoundary(PathBuf),
    /// `GlobalParentLookupLimit` parent levels were searched.
    Limit(u32),
    /// No more parent folders.
    Root,
}

#[derive(Debug, Clone)]
pub struct NotFound {
    pub searched: Vec<PathBuf>,
    pub stop: StopReason,
    /// Global files that exist but aren't valid, with the reason.
    pub rejected: Vec<(PathBuf, String)>,
}

/// Folders to search, in order, and why the list ends.
pub fn search_dirs(container_dir: &Path, limit: u32) -> (Vec<PathBuf>, StopReason) {
    let mut dirs = Vec::new();
    let mut cur = Some(container_dir);
    let mut level = 0;
    while let Some(d) = cur {
        dirs.push(d.to_path_buf());
        if d.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("paks")) {
            return (dirs, StopReason::PaksBoundary(d.to_path_buf()));
        }
        if level == limit {
            return (dirs, StopReason::Limit(limit));
        }
        level += 1;
        cur = d.parent().filter(|p| !p.as_os_str().is_empty());
    }
    (dirs, StopReason::Root)
}

/// Checks `dir\global.utoc`: both files exist, the TOC parses and has script objects.
fn check(dir: &Path) -> Option<Result<GlobalInfo, String>> {
    let utoc = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| {
        p.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("global.utoc")) && p.is_file()
    })?;
    let c = match Container::open(&utoc, crate::profile::profile_for(&utoc)) {
        Ok(c) => c,
        Err(e) => return Some(Err(format!("can't be read: {e}"))),
    };
    if let Some(missing) = c.missing_partitions().first() {
        return Some(Err(format!("{} is missing", missing.display())));
    }
    if !c.has_script_objects() {
        return Some(Err("has no script objects chunk".into()));
    }
    Some(Ok(GlobalInfo { utoc, version: c.version() }))
}

pub fn find_global(container_utoc: &Path, limit: u32) -> Result<GlobalInfo, NotFound> {
    let dir = container_utoc.parent().unwrap_or(Path::new("."));
    let (dirs, stop) = search_dirs(dir, limit);
    let mut rejected = Vec::new();
    for d in &dirs {
        match check(d) {
            Some(Ok(g)) => return Ok(g),
            Some(Err(why)) => rejected.push((d.join("global.utoc"), why)),
            None => {}
        }
    }
    Err(NotFound { searched: dirs, stop, rejected })
}

pub fn not_found_text(container: &Path, nf: &NotFound) -> String {
    let mut s = format!(
        "{}\n\nNo valid global container (global.utoc + global.ucas) was found, so this container can't be opened.\n\nSearched:\n",
        container.display()
    );
    for d in &nf.searched {
        s.push_str(&format!("  {}\n", d.display()));
    }
    s.push_str(&match &nf.stop {
        StopReason::PaksBoundary(p) => format!("The search stops at the Paks folder ({}).", p.display()),
        StopReason::Limit(n) => format!("The search stops after {n} parent folder(s) (GlobalParentLookupLimit in uetoc.ini)."),
        StopReason::Root => "No more parent folders.".to_string(),
    });
    for (p, why) in &nf.rejected {
        s.push_str(&format!("\n{} {}", p.display(), why));
    }
    s.push_str("\n\nCopy the game's global.utoc and global.ucas (from its Content\\Paks folder) next to the container or into a parent folder.");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_stops_at_paks_and_limit() {
        let p = Path::new("/g/Game/Content/Paks/~mods/Sub");
        let (d, s) = search_dirs(p, 5);
        assert_eq!(d.len(), 3);
        assert_eq!(s, StopReason::PaksBoundary(PathBuf::from("/g/Game/Content/Paks")));
        let (d, s) = search_dirs(p, 1);
        assert_eq!(d.len(), 2);
        assert_eq!(s, StopReason::Limit(1));
        let (d, s) = search_dirs(Path::new("/test/a"), 5);
        assert_eq!(d, [PathBuf::from("/test/a"), PathBuf::from("/test"), PathBuf::from("/")]);
        assert_eq!(s, StopReason::Root);
        let (d, s) = search_dirs(Path::new("/x/PAKS"), 0);
        assert_eq!((d.len(), s), (1, StopReason::PaksBoundary(PathBuf::from("/x/PAKS"))));
        assert_eq!(search_dirs(Path::new("/a/b/c"), 0), (vec![PathBuf::from("/a/b/c")], StopReason::Limit(0)));
    }
}
