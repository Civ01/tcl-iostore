//! N7 (`proposals/N7-source-stream.md`): a small NTFS stream `<file>:uetoc.source` on extracted
//! packages, naming the container they came from, so tcl-uassetvw can resolve names (and know
//! the `$legacy` engine version) for files TC extracts to its temp folder for viewing.
//!
//! No side effects on what the plugin produces: written after the file is complete, only on
//! volumes that report named streams (Wine says no but would create a stray `a:uetoc.source`
//! file), only for `.uasset`/`.umap` destination names, with the main file's times put back.
//! Every failure is logged and ignored.

use crate::config::SourceStream;
use crate::dlog;
use std::path::{Path, PathBuf};

pub const STREAM_NAME: &str = "uetoc.source";
pub const FORMAT_TAG: &str = "uetoc-source 1";
pub const MAX_LEN: usize = 4096;

/// What the stream says about one extracted package.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceInfo {
    /// `raw` (Zen package as stored) or `legacy` (converted, `$legacy`).
    pub legacy: bool,
    /// The container's `.utoc`.
    pub container: PathBuf,
    /// Folder of the global container.
    pub paks: PathBuf,
    /// The entry's path in the listing (`$legacy\…` for legacy).
    pub entry: String,
    /// The package's `ExportBundleData` chunk id, 24 hex digits.
    pub chunk: String,
    /// The chunk hash from the TOC (hex).
    pub toc_hash: Option<String>,
    /// `$legacy`: engine version the conversion used (ini spelling).
    pub engine: Option<String>,
    /// First and last engine version that fits the container (`UE5_6-UE5_8`).
    pub engine_range: Option<String>,
}

impl SourceInfo {
    /// The stream's content (§2.3): format tag, then `key=value` lines, LF.
    pub fn render(&self) -> String {
        let mut s = format!("{FORMAT_TAG}\n");
        let mut kv = |k: &str, v: &str| {
            // one line per value: never let a value start a new key
            let v: String = v.chars().map(|c| if c == '\n' || c == '\r' { ' ' } else { c }).collect();
            s.push_str(&format!("{k}={v}\n"));
        };
        kv("form", if self.legacy { "legacy" } else { "raw" });
        kv("container", &self.container.to_string_lossy());
        kv("paks", &self.paks.to_string_lossy());
        kv("entry", &self.entry);
        kv("chunk", &self.chunk);
        if let Some(h) = &self.toc_hash {
            kv("toc_hash", h);
        }
        if let Some(e) = &self.engine {
            kv("engine", e);
        }
        if let Some(r) = &self.engine_range {
            kv("engine_range", r);
        }
        s
    }
}

/// `UE5_6-UE5_8` from a candidate list (one entry: just that version; none: `None`).
pub fn range_text(names: &[&str]) -> Option<String> {
    match names {
        [] => None,
        [one] => Some(one.to_string()),
        [first, .., last] => Some(format!("{first}-{last}")),
    }
}

fn normalized(p: &Path) -> String {
    let s = p.to_string_lossy().replace('/', "\\");
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s).to_string();
    s.trim_end_matches('\\').to_lowercase()
}

/// Whether `dest` is inside TC's view-extraction folder: `<temp>\_tc*\…` (any depth).
pub fn is_view_extraction(dest: &Path, temp: &Path) -> bool {
    let (d, t) = (normalized(dest), normalized(temp));
    let Some(rest) = d.strip_prefix(&t).and_then(|r| r.strip_prefix('\\')) else { return false };
    let mut parts = rest.split('\\');
    let first = parts.next().unwrap_or("");
    first.starts_with("_tc") && parts.next().is_some()
}

/// Whether a stream may be written for `dest` (scope and file name; not the volume).
pub fn wanted(mode: SourceStream, dest: &Path, temp: &Path) -> bool {
    let name = dest.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    if !(name.ends_with(".uasset") || name.ends_with(".umap")) {
        return false;
    }
    match mode {
        SourceStream::Off => false,
        SourceStream::All => true,
        SourceStream::View => is_view_extraction(dest, temp),
    }
}

/// Streams that passed the scope check during tests (any host), with their content.
#[cfg(test)]
pub static TEST_WRITES: std::sync::Mutex<Vec<(PathBuf, SourceInfo)>> = std::sync::Mutex::new(Vec::new());

/// Writes the stream for `dest` when `mode` and the volume allow it. Never fails.
pub fn write(mode: SourceStream, dest: &Path, info: &SourceInfo) {
    if !wanted(mode, dest, &std::env::temp_dir()) {
        return;
    }
    #[cfg(test)]
    TEST_WRITES.lock().unwrap_or_else(|e| e.into_inner()).push((dest.to_path_buf(), info.clone()));
    let text = info.render();
    if text.len() > MAX_LEN {
        dlog!("  source stream: {} bytes, over {MAX_LEN}: not written", text.len());
        return;
    }
    if !imp::supports_streams(dest) {
        dlog!("  source stream: the volume of {} has no named streams", dest.display());
        return;
    }
    match imp::write_keeping_times(dest, text.as_bytes()) {
        Ok(()) => dlog!("  source stream written ({} bytes)", text.len()),
        Err(e) => dlog!("  source stream not written: {e}"),
    }
}

/// `<dest>:uetoc.source`.
pub fn stream_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(":");
    s.push(STREAM_NAME);
    PathBuf::from(s)
}

#[cfg(windows)]
mod imp {
    use std::collections::HashMap;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::sync::Mutex;
    use windows_sys::Win32::Storage::FileSystem::{GetVolumeInformationW, GetVolumePathNameW};

    const FILE_NAMED_STREAMS: u32 = 0x0004_0000;
    static VOLUMES: Mutex<Option<HashMap<String, bool>>> = Mutex::new(None);

    fn wide(p: &Path) -> Vec<u16> {
        p.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    /// The volume of `p` reports named streams (cached per volume).
    pub fn supports_streams(p: &Path) -> bool {
        let mut root = [0u16; 1024];
        if unsafe { GetVolumePathNameW(wide(p).as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
            return false;
        }
        let key = String::from_utf16_lossy(&root[..root.iter().position(|&c| c == 0).unwrap_or(0)]).to_lowercase();
        let mut cache = VOLUMES.lock().unwrap_or_else(|e| e.into_inner());
        let cache = cache.get_or_insert_with(HashMap::new);
        if let Some(&v) = cache.get(&key) {
            return v;
        }
        let mut flags = 0u32;
        let ok = unsafe { GetVolumeInformationW(root.as_ptr(), std::ptr::null_mut(), 0, std::ptr::null_mut(), std::ptr::null_mut(), &mut flags, std::ptr::null_mut(), 0) } != 0;
        let v = ok && flags & FILE_NAMED_STREAMS != 0;
        crate::dlog!("  source stream: volume {key} named streams: {v} (flags {flags:#x})");
        cache.insert(key, v);
        v
    }

    /// Writes the stream, then puts the main file's times back.
    pub fn write_keeping_times(dest: &Path, data: &[u8]) -> std::io::Result<()> {
        use std::os::windows::fs::FileTimesExt;
        let meta = std::fs::metadata(dest)?;
        std::fs::write(super::stream_path(dest), data)?;
        let mut times = std::fs::FileTimes::new();
        if let Ok(t) = meta.modified() {
            times = times.set_modified(t);
        }
        if let Ok(t) = meta.accessed() {
            times = times.set_accessed(t);
        }
        if let Ok(t) = meta.created() {
            times = times.set_created(t);
        }
        std::fs::OpenOptions::new().write(true).open(dest)?.set_times(times)
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::Path;
    /// No named streams outside Windows.
    pub fn supports_streams(_p: &Path) -> bool {
        false
    }
    pub fn write_keeping_times(_dest: &Path, _data: &[u8]) -> std::io::Result<()> {
        Err(std::io::Error::other("no named streams"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_scope_is_tcs_temp_folder() {
        let t = Path::new(r"C:\Users\u\AppData\Local\Temp");
        assert!(is_view_extraction(Path::new(r"C:\Users\u\AppData\Local\Temp\_tc\BP_SML.uasset"), t));
        assert!(is_view_extraction(Path::new(r"c:\users\u\appdata\local\temp\_TC1\sub\BP.umap"), t));
        assert!(is_view_extraction(Path::new(r"\\?\C:\Users\u\AppData\Local\Temp\_tc\a.uasset"), Path::new(r"C:\Users\u\AppData\Local\Temp\")));
        assert!(!is_view_extraction(Path::new(r"C:\Users\u\AppData\Local\Temp\WCSdc76.tmp"), t), "text search");
        assert!(!is_view_extraction(Path::new(r"C:\Users\u\AppData\Local\Temp\x\_tc\a.uasset"), t));
        assert!(!is_view_extraction(Path::new(r"C:\Users\u\AppData\Local\Temp\_tc"), t));
        assert!(!is_view_extraction(Path::new(r"D:\mods\_tc\a.uasset"), t));
        assert!(!is_view_extraction(Path::new(r"C:\Users\u\AppData\Local\Temp2\_tc\a.uasset"), t));
    }

    #[test]
    fn only_package_names_and_the_mode() {
        let t = Path::new(r"C:\T");
        let view = Path::new(r"C:\T\_tc\a.uasset");
        let f5 = Path::new(r"D:\out\Game\a.UMAP");
        assert!(wanted(SourceStream::View, view, t));
        assert!(!wanted(SourceStream::View, f5, t));
        assert!(wanted(SourceStream::All, f5, t));
        assert!(!wanted(SourceStream::Off, view, t));
        for name in [r"C:\T\_tc\a.uexp", r"C:\T\_tc\a.ubulk", r"C:\T\WCSdc76.tmp", r"C:\T\_tc\x.txt"] {
            assert!(!wanted(SourceStream::All, Path::new(name), t), "{name}");
        }
    }

    #[test]
    fn content_follows_the_format() {
        let i = SourceInfo {
            legacy: true,
            container: r"C:\Games\Moadra\Content\Paks\SML.utoc".into(),
            paks: r"C:\Games\Moadra\Content\Paks".into(),
            entry: "$legacy\\Moadra\\Content\\Mods\\SML\\BP_SML.uasset".into(),
            chunk: "5987b788f2a920550000000001".into(),
            toc_hash: Some("ab12".into()),
            engine: Some("UE5_8".into()),
            engine_range: Some("UE5_6-UE5_8".into()),
        };
        let s = i.render();
        assert!(s.starts_with("uetoc-source 1\nform=legacy\ncontainer=C:\\Games"), "{s}");
        assert!(s.ends_with("engine=UE5_8\nengine_range=UE5_6-UE5_8\n"), "{s}");
        assert!(!s.contains('\r'));
        let raw = SourceInfo { legacy: false, toc_hash: None, engine: None, engine_range: None, ..i };
        let r = raw.render();
        assert!(r.contains("form=raw\n") && !r.contains("engine") && !r.contains("toc_hash"), "{r}");
        let odd = SourceInfo { entry: "a\nform=x".into(), ..Default::default() };
        assert_eq!(odd.render().matches("\nform=").count(), 1, "a value can't start a line");
        assert_eq!(range_text(&["UE5_6", "UE5_7", "UE5_8"]).as_deref(), Some("UE5_6-UE5_8"));
        assert_eq!(range_text(&["UE4_27"]).as_deref(), Some("UE4_27"));
        assert_eq!(range_text(&[]), None);
        assert_eq!(stream_path(Path::new(r"C:\a\b.uasset")), PathBuf::from(r"C:\a\b.uasset:uetoc.source"));
    }

    #[test]
    fn nothing_is_written_where_streams_are_unsupported() {
        // on this host (or a volume without streams) the main file stays exactly as it was
        let dir = std::env::temp_dir().join(format!("uetoc-n7-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.uasset");
        std::fs::write(&f, b"main").unwrap();
        let before = std::fs::metadata(&f).unwrap().modified().unwrap();
        if !imp::supports_streams(&f) {
            write(SourceStream::All, &f, &SourceInfo::default());
            let names: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
            assert_eq!(names.len(), 1, "no stray file: {names:?}");
        }
        assert_eq!(std::fs::read(&f).unwrap(), b"main");
        assert_eq!(std::fs::metadata(&f).unwrap().modified().unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Windows CI (NTFS temp folder): the stream is written, reads back, and the main file's
    /// bytes and times are unchanged.
    #[cfg(windows)]
    #[test]
    fn stream_round_trips_and_keeps_the_file() {
        let dir = std::env::temp_dir().join(format!("uetoc-n7w-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.uasset");
        std::fs::write(&f, b"main content").unwrap();
        let m = std::fs::metadata(&f).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        // NTFS temp folder expected (GitHub's Windows runners): fail instead of skipping silently
        assert!(imp::supports_streams(&f), "the temp volume reports no named streams: {}", f.display());
        let info = SourceInfo { chunk: "00".repeat(12), entry: "Game\\a.uasset".into(), ..Default::default() };
        write(SourceStream::All, &f, &info);
        assert_eq!(std::fs::read_to_string(stream_path(&f)).unwrap(), info.render());
        assert_eq!(std::fs::read(&f).unwrap(), b"main content");
        let n = std::fs::metadata(&f).unwrap();
        assert_eq!(n.len(), m.len());
        assert_eq!(n.modified().unwrap(), m.modified().unwrap());
        assert_eq!(n.created().unwrap(), m.created().unwrap());
        // View: not under _tc → nothing
        let g = dir.join("b.uasset");
        std::fs::write(&g, b"x").unwrap();
        write(SourceStream::View, &g, &info);
        assert!(std::fs::metadata(stream_path(&g)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
