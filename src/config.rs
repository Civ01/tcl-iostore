//! Plugin locations and options (`uetoc.ini`, SPEC §9).
//!
//! The settings directory is the plugin's own directory when it is writable,
//! otherwise the directory of TC's suggested ini (next to wincmd.ini), as in tcl-uepak.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

pub const INI_NAME: &str = "uetoc.ini";
pub const DEPS_DIR: &str = "deps";

/// How the container's mount point appears in the listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MountPointMode {
    /// Paths are shown relative to the mount point.
    Root,
    /// The mount point (minus leading `../`) is shown as top-level folders.
    #[default]
    Folder,
}

pub const ATTR_READONLY: u32 = 0x1;
pub const ATTR_HIDDEN: u32 = 0x2;
pub const ATTR_SYSTEM: u32 = 0x4;

pub const DEFAULT_GLOBAL_LOOKUP_LIMIT: u32 = 5;

/// `[Pack] ValidateOnAdd` (SPEC §6c): checks for files the game doesn't have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ValidateOnAdd {
    /// List the findings in the additions question.
    #[default]
    Warn,
    /// Move files to the path their name gives (with their bulk files).
    Fix,
    /// Refuse the copy when anything is found.
    Error,
    /// No checks.
    Off,
}

/// `[Pack] Compression`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PackCompression {
    /// The container's own method (new containers: the game's most used one).
    #[default]
    Match,
    None,
    Zlib,
    Gzip,
    Lz4,
    Oodle,
}

/// `[Options] SourceStream` (N7, `proposals/N7-source-stream.md`): which extracted packages get a
/// `<file>:uetoc.source` stream naming their container, for tcl-uassetvw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceStream {
    /// Only TC's view extractions (F3, Quick View, Enter: `%TEMP%\_tc*\…`).
    View,
    /// Every package extraction.
    All,
    Off,
}

/// `[Pack] ValidateOnReplace`: what to do when a packed file takes the place of a game package
/// and differs from it in ways that can break loading (SPEC §6b.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidateOnReplace {
    /// Ask before packing (list of findings, Yes/No).
    Warn,
    /// Correct what can be corrected (store entry imports, 5.3+), pack, then list what was done.
    Fix,
    /// Refuse the whole copy.
    Error,
    /// No checks.
    Off,
}

/// `[Pack]` options.
#[derive(Debug, Clone)]
pub struct PackOptions {
    pub compression: PackCompression,
    /// Zlib/gzip level 0–9.
    pub zlib_level: u32,
    pub oodle_compressor: iostore::write::OodleCompressor,
    pub oodle_level: iostore::write::OodleLevel,
    /// New containers: write an empty companion `.pak` (if none exists).
    pub write_empty_pak: bool,
    /// New containers without a `.pak`: warn that one is needed.
    pub warn_pak_needed: bool,
    /// Always rebuild after a change (drops dead space).
    pub compact: bool,
    /// Rebuild containers smaller than this after a change (0 = never).
    pub auto_compact_under: u64,
    /// New containers: ask for the mount point every time (else the last one is used).
    pub ask_mount_point: bool,
    /// How many mount points the dialog remembers (0 = none).
    pub mount_point_history: usize,
    /// Last mount points, newest first: `Auto` or `../../../…/` (saved by the plugin).
    pub recent_mount_points: Vec<String>,
    pub validate_on_replace: ValidateOnReplace,
    pub validate_on_add: ValidateOnAdd,
}

impl Default for PackOptions {
    fn default() -> Self {
        PackOptions {
            compression: PackCompression::default(),
            zlib_level: 6,
            oodle_compressor: iostore::write::OodleCompressor::Kraken,
            oodle_level: iostore::write::OodleLevel::Normal,
            write_empty_pak: true,
            warn_pak_needed: true,
            compact: false,
            auto_compact_under: 10 << 20,
            ask_mount_point: true,
            mount_point_history: 10,
            recent_mount_points: Vec::new(),
            validate_on_replace: ValidateOnReplace::Warn,
            validate_on_add: ValidateOnAdd::Warn,
        }
    }
}

/// `10M`, `512K`, `1G`, plain bytes.
pub fn parse_size(v: &str) -> Option<u64> {
    let v = v.trim();
    let (num, mul) = match v.chars().last().map(|c| c.to_ascii_uppercase()) {
        Some('K') => (&v[..v.len() - 1], 1u64 << 10),
        Some('M') => (&v[..v.len() - 1], 1 << 20),
        Some('G') => (&v[..v.len() - 1], 1 << 30),
        _ => (v, 1),
    };
    num.trim().parse::<u64>().ok().and_then(|n| n.checked_mul(mul))
}
pub const MAX_GLOBAL_LOOKUP_LIMIT: u32 = 64;

#[derive(Debug, Clone)]
pub struct Options {
    /// `OpenUtocAsVirtualFile=1`: opening a `.utoc` shows only `<ContainerId>.txt`.
    pub open_utoc_as_virtual_file: bool,
    /// Parent folders searched for `global.utoc` (the `Paks` boundary always applies).
    pub global_lookup_limit: u32,
    pub mount_point: MountPointMode,
    pub mount_folder_attr: u32,
    /// Attribute of the `$chunks` folder.
    pub virtual_folder_attr: u32,
    pub debug_log: bool,
    /// `LegacyEngineVersion`: engine version for unversioned packages in `$legacy`; `None` = Auto.
    pub legacy_engine_version: Option<retoc::version::EngineVersion>,
    /// `LegacyGuessScriptTypes`: name engine structs/enums in `$legacy` imports by a name guess
    /// instead of `Object` (a wrong guess makes the legacy loader fail that import).
    pub legacy_guess_script_types: bool,
    /// `SourceStream` (N7): where extracted packages get a `:uetoc.source` stream.
    pub source_stream: SourceStream,
    /// `[Pack]` section.
    pub pack: PackOptions,
    /// Problems found while parsing (logged; invalid values keep defaults).
    pub warnings: Vec<String>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            open_utoc_as_virtual_file: true,
            global_lookup_limit: DEFAULT_GLOBAL_LOOKUP_LIMIT,
            mount_point: MountPointMode::default(),
            mount_folder_attr: ATTR_HIDDEN,
            virtual_folder_attr: ATTR_HIDDEN,
            debug_log: false,
            legacy_engine_version: None,
            legacy_guess_script_types: false,
            source_stream: SourceStream::View,
            pack: PackOptions::default(),
            warnings: vec![],
        }
    }
}

pub fn parse_attr_list(value: &str) -> u32 {
    value
        .split([',', '|', '+', ' '])
        .map(|v| match v.trim().to_ascii_lowercase().as_str() {
            "readonly" | "r" => ATTR_READONLY,
            "hidden" | "h" => ATTR_HIDDEN,
            "system" | "s" => ATTR_SYSTEM,
            _ => 0,
        })
        .fold(0, |a, b| a | b)
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" | "" => Some(false),
        _ => None,
    }
}

static PLUGIN_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
static TC_INI_DIR: RwLock<Option<PathBuf>> = RwLock::new(None);

pub fn plugin_dir() -> Option<&'static Path> {
    PLUGIN_DIR.get_or_init(|| crate::win::module_path().and_then(|p| p.parent().map(Path::to_path_buf))).as_deref()
}

pub fn deps_dir() -> Option<PathBuf> {
    plugin_dir().map(|d| d.join(DEPS_DIR))
}

pub fn set_tc_ini_name(ini: &Path) {
    if let Some(dir) = ini.parent() {
        *TC_INI_DIR.write().unwrap_or_else(|e| e.into_inner()) = Some(dir.to_path_buf());
    }
}

fn tc_ini_dir() -> Option<PathBuf> {
    TC_INI_DIR.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Can we create files in `dir`? (Program Files denies writes to non-admins.)
fn is_writable_dir(dir: &Path) -> bool {
    let probe = dir.join(".uetoc-write-test");
    match std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// A remembered mount point: `Auto`, or `../../../…/` with `/` separators.
pub fn normalize_recent(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    if t.eq_ignore_ascii_case("auto") {
        return Some("Auto".into());
    }
    let mut m = t.replace('\\', "/");
    if m.contains(['"', '*', '?', '<', '>', '|', ':']) {
        return None;
    }
    if !m.ends_with('/') {
        m.push('/');
    }
    Some(m)
}

/// Directory for writable plugin data (options, remembered AES keys).
pub fn settings_dir() -> Option<PathBuf> {
    match plugin_dir() {
        Some(dir) if is_writable_dir(dir) => Some(dir.to_path_buf()),
        _ => tc_ini_dir().or_else(|| plugin_dir().map(Path::to_path_buf)),
    }
}

/// The ini file in use (settings dir first, then the shipped default in the plugin dir).
pub fn ini_path() -> Option<PathBuf> {
    [settings_dir(), plugin_dir().map(Path::to_path_buf)].into_iter().flatten().map(|d| d.join(INI_NAME)).find(|p| p.is_file())
}

/// Loads options; missing file or keys fall back to defaults.
pub fn load_options() -> Options {
    match ini_path().and_then(|p| std::fs::read_to_string(p).ok()) {
        Some(text) => parse_options(&text),
        None => Options::default(),
    }
}

/// Sets `[Options] key=value` in the settings directory's ini, keeping everything else
/// (a shipped default in the plugin folder is copied first).
pub fn set_option(key: &str, value: &str) -> std::io::Result<()> {
    set_values("Options", &[(key, value.to_string())]).map(|_| ())
}

/// Sets several `key=value` lines of one section in the settings directory's ini (as
/// `set_option`). Returns the file written.
pub fn set_values(section: &str, values: &[(&str, String)]) -> std::io::Result<PathBuf> {
    let dir = settings_dir().ok_or_else(|| std::io::Error::other("no settings folder"))?;
    let path = dir.join(INI_NAME);
    let mut text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => ini_path().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default(),
    };
    for (k, v) in values {
        text = set_value_text(&text, section, k, v);
    }
    std::fs::write(&path, text)?;
    Ok(path)
}

pub fn set_option_text(text: &str, key: &str, value: &str) -> String {
    set_value_text(text, "Options", key, value)
}

/// `text` with `[section] key=value` set: the existing line keeps its inline comment (in its
/// column), a missing key goes at the end of the section, a missing section at the end of the
/// file.
pub fn set_value_text(text: &str, section_name: &str, key: &str, value: &str) -> String {
    let norm = |k: &str| k.trim().to_ascii_lowercase().replace('_', "");
    let want = norm(key);
    let target = section_name.to_ascii_lowercase();
    // the comment column most `key=value   ; comment` lines use (for a comment that a long
    // value had pushed right)
    let mut columns: HashMap<usize, usize> = HashMap::new();
    for line in text.lines() {
        if let Some(i) = line.find(" ;").filter(|_| !line.contains('\t') && line.contains('=')) {
            if line[..i].ends_with(' ') {
                *columns.entry(i + 1).or_default() += 1;
            }
        }
    }
    let usual_column = columns.into_iter().max_by_key(|&(c, n)| (n, c)).map(|(c, _)| c);
    let mut out: Vec<String> = Vec::new();
    let mut section = String::new();
    let mut section_end: Option<usize> = None;
    let mut done = false;
    for line in text.lines() {
        let t = line.trim();
        if let Some(name) = t.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            if section == target && section_end.is_none() {
                section_end = Some(out.len());
            }
            section = name.trim().to_ascii_lowercase();
            out.push(line.to_string());
            continue;
        }
        if section == target && !done && !t.starts_with(';') && !t.starts_with('#') {
            if let Some((k, rest)) = line.split_once('=') {
                if norm(k) == want {
                    // keep an inline comment, in its column when the value fits (spaces only)
                    let head = format!("{}={value}", k.trim_end());
                    let line_out = match rest.find(';') {
                        Some(i) if i > 0 && rest[..i].ends_with(' ') && !line.contains('\t') => {
                            let column = match usual_column {
                                // pushed right by an earlier long value: back to the usual column
                                Some(c) if !rest[..i].ends_with("  ") => c,
                                _ => k.len() + 1 + i,
                            };
                            format!("{head:<width$}{}", &rest[i..], width = column.max(head.len() + 1))
                        }
                        Some(i) if i > 0 && rest[..i].ends_with([' ', '\t']) => format!("{head}{}", &rest[rest[..i].trim_end().len()..]),
                        _ => head,
                    };
                    out.push(line_out);
                    done = true;
                    continue;
                }
            }
        }
        out.push(line.to_string());
    }
    if !done {
        let line = format!("{key}={value}");
        match (section == target, section_end) {
            (_, Some(i)) => {
                // before blank lines that end the section
                let mut i = i;
                while i > 0 && out[i - 1].trim().is_empty() {
                    i -= 1;
                }
                out.insert(i, line)
            }
            (true, None) => out.push(line),
            (false, None) => {
                if out.last().is_some_and(|l| !l.trim().is_empty()) {
                    out.push(String::new());
                }
                out.push(format!("[{section_name}]"));
                out.push(line);
            }
        }
    }
    let mut s = out.join("\r\n");
    s.push_str("\r\n");
    s
}

pub fn parse_options(text: &str) -> Options {
    let mut opts = Options::default();
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim().to_ascii_lowercase();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        // inline comments: "Key=Value   ; comment"
        let value = match value.find([';', '#']) {
            Some(i) if i > 0 && value[..i].ends_with([' ', '\t']) => &value[..i],
            _ => value,
        };
        // Keys are case-insensitive and ignore '_' (DebugLog == debug_log).
        let key = key.trim().to_ascii_lowercase().replace('_', "");
        let raw = value.trim();
        if section == "pack" {
            parse_pack_option(&mut opts, &key, raw);
            continue;
        }
        if section != "options" {
            continue;
        }
        let mut bad = |k: &str| opts.warnings.push(format!("[Options] {k}={raw}: invalid, default kept"));
        match key.as_str() {
            "openutocasvirtualfile" => match parse_bool(raw) {
                Some(b) => opts.open_utoc_as_virtual_file = b,
                None => bad(&key),
            },
            "globalparentlookuplimit" => match raw.parse::<u32>() {
                Ok(n) if n <= MAX_GLOBAL_LOOKUP_LIMIT => opts.global_lookup_limit = n,
                _ => bad(&key),
            },
            "mountpoint" => {
                opts.mount_point = match raw.to_ascii_lowercase().as_str() {
                    "root" | "0" => MountPointMode::Root,
                    _ => MountPointMode::Folder,
                }
            }
            "mountfolderattr" => opts.mount_folder_attr = parse_attr_list(raw),
            "virtualfolderattr" => opts.virtual_folder_attr = parse_attr_list(raw),
            "debuglog" => opts.debug_log = parse_bool(raw).unwrap_or(false),
            "legacyguessscripttypes" => opts.legacy_guess_script_types = parse_bool(raw).unwrap_or(false),
            "sourcestream" => match raw.to_ascii_lowercase().as_str() {
                "view" => opts.source_stream = SourceStream::View,
                "all" => opts.source_stream = SourceStream::All,
                "off" | "0" | "no" => opts.source_stream = SourceStream::Off,
                _ => bad(&key),
            },
            "legacyengineversion" => match crate::legacy::parse_version(raw) {
                Ok(v) => opts.legacy_engine_version = v,
                Err(()) => bad(&key),
            },
            _ => {}
        }
    }
    opts
}

fn parse_pack_option(opts: &mut Options, key: &str, raw: &str) {
    use iostore::write::{OodleCompressor as C, OodleLevel as L};
    let lower = raw.to_ascii_lowercase();
    let p = &mut opts.pack;
    let ok = match key {
        // removed with N1b (additions always ask once per copy): old lines are ignored
        "packmode" => true,
        "validateonadd" => {
            let v = match lower.as_str() {
                "warn" => Some(ValidateOnAdd::Warn),
                "fix" => Some(ValidateOnAdd::Fix),
                "error" => Some(ValidateOnAdd::Error),
                "off" => Some(ValidateOnAdd::Off),
                _ => None,
            };
            v.map(|v| p.validate_on_add = v).is_some()
        }
        "compression" => {
            let c = match lower.as_str() {
                "match" | "auto" => Some(PackCompression::Match),
                "none" | "store" => Some(PackCompression::None),
                "zlib" => Some(PackCompression::Zlib),
                "gzip" => Some(PackCompression::Gzip),
                "lz4" => Some(PackCompression::Lz4),
                "oodle" => Some(PackCompression::Oodle),
                _ => None,
            };
            c.map(|c| p.compression = c).is_some()
        }
        "validateonreplace" => {
            let v = match lower.as_str() {
                "warn" => Some(ValidateOnReplace::Warn),
                "fix" => Some(ValidateOnReplace::Fix),
                "error" => Some(ValidateOnReplace::Error),
                "off" => Some(ValidateOnReplace::Off),
                _ => None,
            };
            v.map(|v| p.validate_on_replace = v).is_some()
        }
        "zliblevel" => lower.parse::<u32>().ok().filter(|l| *l <= 9).map(|l| p.zlib_level = l).is_some(),
        "oodlecompressor" => {
            let c = match lower.as_str() {
                "kraken" => Some(C::Kraken),
                "mermaid" => Some(C::Mermaid),
                "selkie" => Some(C::Selkie),
                "leviathan" => Some(C::Leviathan),
                _ => None,
            };
            c.map(|c| p.oodle_compressor = c).is_some()
        }
        "oodlelevel" => {
            let l = match lower.as_str() {
                "superfast" | "1" => Some(L::SuperFast),
                "veryfast" | "2" => Some(L::VeryFast),
                "fast" | "3" => Some(L::Fast),
                "normal" | "4" => Some(L::Normal),
                "optimal1" | "5" => Some(L::Optimal1),
                "optimal2" | "6" => Some(L::Optimal2),
                "optimal3" | "7" => Some(L::Optimal3),
                "optimal4" | "8" => Some(L::Optimal4),
                "optimal5" | "9" => Some(L::Optimal5),
                _ => None,
            };
            l.map(|l| p.oodle_level = l).is_some()
        }
        "writeemptypak" => parse_bool(raw).map(|b| p.write_empty_pak = b).is_some(),
        "warnpakneeded" => parse_bool(raw).map(|b| p.warn_pak_needed = b).is_some(),
        "compact" => parse_bool(raw).map(|b| p.compact = b).is_some(),
        "autocompactunder" => parse_size(raw).map(|n| p.auto_compact_under = n).is_some(),
        "askmountpoint" => parse_bool(raw).map(|b| p.ask_mount_point = b).is_some(),
        "mountpointhistory" => raw.trim().parse::<usize>().ok().filter(|n| *n <= 50).map(|n| p.mount_point_history = n).is_some(),
        "recentmountpoints" => {
            for m in raw.split('|').filter_map(normalize_recent) {
                if !p.recent_mount_points.iter().any(|r| r.eq_ignore_ascii_case(&m)) {
                    p.recent_mount_points.push(m);
                }
            }
            true
        }
        // before the mount-point dialog: becomes the oldest remembered entry
        "newmountpoint" | "mountpoint" => {
            if let Some(m) = normalize_recent(raw).filter(|m| m != "Auto") {
                if !p.recent_mount_points.iter().any(|r| r.eq_ignore_ascii_case(&m)) {
                    p.recent_mount_points.push(m);
                }
            }
            true
        }
        _ => true,
    };
    if !ok {
        opts.warnings.push(format!("[Pack] {key}={raw}: invalid, default kept"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_values() {
        let d = parse_options("");
        assert!(d.open_utoc_as_virtual_file);
        assert_eq!(d.global_lookup_limit, 5);
        assert_eq!(d.mount_point, MountPointMode::Folder);
        assert_eq!((d.mount_folder_attr, d.virtual_folder_attr), (ATTR_HIDDEN, ATTR_HIDDEN));
        let o = parse_options(
            "[Options]\nOpenUtocAsVirtualFile=0 ; c\nGlobalParentLookupLimit = 2\nMountPoint=Root\nVirtualFolderAttr=None\ndebug_log=yes",
        );
        assert!(!o.open_utoc_as_virtual_file);
        assert_eq!(o.global_lookup_limit, 2);
        assert_eq!(o.mount_point, MountPointMode::Root);
        assert_eq!(o.virtual_folder_attr, 0);
        assert!(o.debug_log);
        let bad = parse_options("[Options]\nGlobalParentLookupLimit=999\nOpenUtocAsVirtualFile=maybe");
        assert_eq!(bad.global_lookup_limit, 5);
        assert_eq!(bad.warnings.len(), 2);
        assert!(!parse_options("[Other]\nDebugLog=1").debug_log);
        assert_eq!(parse_options("[Options]\nLegacyEngineVersion=UE5_7").legacy_engine_version, Some(retoc::version::EngineVersion::UE5_7));
        assert_eq!(parse_options("[Options]\nLegacyEngineVersion=Auto").legacy_engine_version, None);
        assert!(!parse_options("[Options]").legacy_guess_script_types);
        assert!(parse_options("[Options]\nLegacyGuessScriptTypes=1").legacy_guess_script_types);
        let o = parse_options("[Pack]\nPackMode=ReplaceWithAddition\nValidateOnAdd=Fix\nCompression=Oodle ; c\nOodleLevel=Optimal2\nWriteEmptyPak=0\nAutoCompactUnder=2M\nNewMountPoint=..\\..\\..\\Game\\");
        assert!(o.warnings.is_empty(), "old PackMode lines are ignored: {:?}", o.warnings);
        let p = o.pack;
        assert_eq!(p.validate_on_add, ValidateOnAdd::Fix);
        assert_eq!(p.compression, PackCompression::Oodle);
        assert_eq!(p.oodle_level, iostore::write::OodleLevel::Optimal2);
        assert!(!p.write_empty_pak && p.warn_pak_needed);
        assert_eq!(p.auto_compact_under, 2 << 20);
        assert_eq!(p.recent_mount_points, ["../../../Game/"]);
        let p = parse_options("[Pack]\nNewMountPoint=../../../Old\nRecentMountPoints=auto|..\\..\\..\\A| ../../../Old/ \nMountPointHistory=3\nAskMountPoint=0").pack;
        assert_eq!(p.recent_mount_points, ["../../../Old/", "Auto", "../../../A/"], "file order; NewMountPoint first here as it comes first");
        assert_eq!((p.mount_point_history, p.ask_mount_point), (3, false));
        let d = parse_options("").pack;
        assert_eq!((d.validate_on_add, d.compression, d.auto_compact_under), (ValidateOnAdd::Warn, PackCompression::Match, 10 << 20));
        assert_eq!(parse_options("[Pack]\nCompression=Zstd").warnings.len(), 1);
    }

    #[test]
    fn set_option_keeps_the_rest() {
        let t = set_option_text("; top\n[Options]\nDebugLog=0\nLegacyEngineVersion=Auto    ; c\n\n[Other]\nX=1\n", "LegacyEngineVersion", "UE5_7");
        assert_eq!(t, "; top\r\n[Options]\r\nDebugLog=0\r\nLegacyEngineVersion=UE5_7   ; c\r\n\r\n[Other]\r\nX=1\r\n");
        let t = set_option_text("[Options]\nDebugLog=0\n\n[Other]\nX=1", "LegacyEngineVersion", "UE5_6");
        assert_eq!(t, "[Options]\r\nDebugLog=0\r\nLegacyEngineVersion=UE5_6\r\n\r\n[Other]\r\nX=1\r\n");
        assert_eq!(set_option_text("", "A", "1"), "[Options]\r\nA=1\r\n");
        assert_eq!(set_option_text("[Options]\nB=2", "A", "1"), "[Options]\r\nB=2\r\nA=1\r\n");
        assert_eq!(parse_options(&t).legacy_engine_version, Some(retoc::version::EngineVersion::UE5_6));
    }
}
