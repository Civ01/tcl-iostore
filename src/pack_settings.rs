//! The packing part of the Configure dialog (SPEC §8): `[Pack]` options as dialog choices and
//! back, with validation, and the ini lines written on OK. The Win32 dialog itself is in
//! `dialog.rs`; everything here is plain data so it is unit-tested.

use crate::config::{parse_size, PackCompression, PackOptions, ValidateOnAdd, ValidateOnReplace};
use iostore::write::{OodleCompressor, OodleLevel};

pub const ADD_VALIDATIONS: [(&str, ValidateOnAdd); 4] = [
    ("Warn: list problems in the question", ValidateOnAdd::Warn),
    ("Fix: move files to the path their name gives", ValidateOnAdd::Fix),
    ("Error: refuse the copy", ValidateOnAdd::Error),
    ("Off: no checks", ValidateOnAdd::Off),
];

pub const COMPRESSIONS: [(&str, PackCompression); 6] = [
    ("Match the game", PackCompression::Match),
    ("None", PackCompression::None),
    ("Zlib", PackCompression::Zlib),
    ("Gzip", PackCompression::Gzip),
    ("LZ4", PackCompression::Lz4),
    ("Oodle", PackCompression::Oodle),
];

pub const OODLE_COMPRESSORS: [(&str, OodleCompressor); 4] = [
    ("Kraken", OodleCompressor::Kraken),
    ("Mermaid", OodleCompressor::Mermaid),
    ("Selkie", OodleCompressor::Selkie),
    ("Leviathan", OodleCompressor::Leviathan),
];

pub const OODLE_LEVELS: [(&str, OodleLevel); 9] = [
    ("SuperFast", OodleLevel::SuperFast),
    ("VeryFast", OodleLevel::VeryFast),
    ("Fast", OodleLevel::Fast),
    ("Normal", OodleLevel::Normal),
    ("Optimal1", OodleLevel::Optimal1),
    ("Optimal2", OodleLevel::Optimal2),
    ("Optimal3", OodleLevel::Optimal3),
    ("Optimal4", OodleLevel::Optimal4),
    ("Optimal5", OodleLevel::Optimal5),
];

pub const VALIDATIONS: [(&str, ValidateOnReplace); 4] = [
    ("Warn: ask before packing", ValidateOnReplace::Warn),
    ("Fix: correct the imports, then list the fixes", ValidateOnReplace::Fix),
    ("Error: refuse the copy", ValidateOnReplace::Error),
    ("Off: no checks", ValidateOnReplace::Off),
];

pub const ZLIB_LEVELS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];

/// What the dialog shows: list indices, check boxes and edit texts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackForm {
    /// ValidateOnAdd.
    pub validate_add: usize,
    pub compression: usize,
    pub zlib_level: usize,
    pub oodle_compressor: usize,
    pub oodle_level: usize,
    pub compact: bool,
    pub auto_compact_under: String,
    pub write_empty_pak: bool,
    pub warn_pak_needed: bool,
    /// New containers: ask for the mount point.
    pub ask_mount_point: bool,
    /// How many mount points to remember (text, 0-50).
    pub mount_point_history: String,
    pub validate: usize,
}

fn index_of<T: PartialEq + Copy>(list: &[(&str, T)], v: T) -> usize {
    list.iter().position(|(_, x)| *x == v).unwrap_or(0)
}

impl PackForm {
    pub fn from_options(p: &PackOptions) -> Self {
        PackForm {
            validate_add: index_of(&ADD_VALIDATIONS, p.validate_on_add),
            compression: index_of(&COMPRESSIONS, p.compression),
            zlib_level: (p.zlib_level as usize).min(9),
            oodle_compressor: index_of(&OODLE_COMPRESSORS, p.oodle_compressor),
            oodle_level: index_of(&OODLE_LEVELS, p.oodle_level),
            compact: p.compact,
            auto_compact_under: format_size(p.auto_compact_under),
            write_empty_pak: p.write_empty_pak,
            warn_pak_needed: p.warn_pak_needed,
            ask_mount_point: p.ask_mount_point,
            mount_point_history: p.mount_point_history.to_string(),
            validate: index_of(&VALIDATIONS, p.validate_on_replace),
        }
    }

    /// The options, or a message for the user (the dialog stays open).
    pub fn to_options(&self) -> Result<PackOptions, String> {
        let pick = |i: usize, n: usize, what: &str| {
            if i < n {
                Ok(i)
            } else {
                Err(format!("Pick a {what}."))
            }
        };
        let auto_compact_under = parse_size(&self.auto_compact_under).ok_or_else(|| {
            format!(
                "\"{}\" is not a size: use bytes or K/M/G, e.g. 10M (0 = never).",
                self.auto_compact_under.trim()
            )
        })?;
        let mount_point_history = self.mount_point_history.trim().parse::<usize>().ok().filter(|n| *n <= 50).ok_or_else(|| {
            format!("\"{}\" is not a number of mount points to remember (0-50).", self.mount_point_history.trim())
        })?;
        Ok(PackOptions {
            validate_on_add: ADD_VALIDATIONS[pick(self.validate_add, ADD_VALIDATIONS.len(), "check for added files")?].1,
            compression: COMPRESSIONS[pick(self.compression, COMPRESSIONS.len(), "compression")?].1,
            zlib_level: pick(self.zlib_level, ZLIB_LEVELS.len(), "Zlib level")? as u32,
            oodle_compressor: OODLE_COMPRESSORS[pick(
                self.oodle_compressor,
                OODLE_COMPRESSORS.len(),
                "Oodle compressor",
            )?]
            .1,
            oodle_level: OODLE_LEVELS[pick(self.oodle_level, OODLE_LEVELS.len(), "Oodle level")?].1,
            compact: self.compact,
            auto_compact_under,
            write_empty_pak: self.write_empty_pak,
            warn_pak_needed: self.warn_pak_needed,
            ask_mount_point: self.ask_mount_point,
            mount_point_history,
            recent_mount_points: Vec::new(),
            validate_on_replace: VALIDATIONS[pick(self.validate, VALIDATIONS.len(), "validation")?].1,
        })
    }

    /// Whether the Zlib level list applies to the chosen compression.
    pub fn zlib_level_used(&self) -> bool {
        matches!(
            COMPRESSIONS.get(self.compression).map(|c| c.1),
            Some(PackCompression::Zlib | PackCompression::Gzip | PackCompression::Match)
        )
    }

    /// Whether the Oodle lists apply to the chosen compression.
    pub fn oodle_used(&self) -> bool {
        matches!(
            COMPRESSIONS.get(self.compression).map(|c| c.1),
            Some(PackCompression::Oodle | PackCompression::Match)
        )
    }
}

/// `None` for Auto; else `../../../Game/Content/` style (forward slashes, trailing slash).
pub(crate) fn normalize_mount_point(s: &str) -> Result<Option<String>, String> {
    let t = s.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("auto") {
        return Ok(None);
    }
    let mut m = t.replace('\\', "/");
    if m.contains(['"', '*', '?', '<', '>', '|', ':']) {
        return Err(format!(
            "\"{t}\" is not a mount point (example: ../../../Game/Content/)."
        ));
    }
    if !m.ends_with('/') {
        m.push('/');
    }
    Ok(Some(m))
}

/// `10M` for 10 MiB; plain bytes when not a whole K/M/G.
pub fn format_size(n: u64) -> String {
    for (unit, shift) in [("G", 30), ("M", 20), ("K", 10)] {
        if n != 0 && n.is_multiple_of(1 << shift) {
            return format!("{}{unit}", n >> shift);
        }
    }
    n.to_string()
}

/// The `[Pack]` lines for `p`, in the names and spellings `uetoc.ini` uses.
pub fn ini_values(p: &PackOptions) -> Vec<(&'static str, String)> {
    let add = match p.validate_on_add {
        ValidateOnAdd::Warn => "Warn",
        ValidateOnAdd::Fix => "Fix",
        ValidateOnAdd::Error => "Error",
        ValidateOnAdd::Off => "Off",
    };
    let compression = match p.compression {
        PackCompression::Match => "Match",
        PackCompression::None => "None",
        PackCompression::Zlib => "Zlib",
        PackCompression::Gzip => "Gzip",
        PackCompression::Lz4 => "LZ4",
        PackCompression::Oodle => "Oodle",
    };
    let b = |v: bool| if v { "1" } else { "0" }.to_string();
    vec![
        ("ValidateOnAdd", add.into()),
        ("Compression", compression.into()),
        ("ZlibLevel", p.zlib_level.to_string()),
        (
            "OodleCompressor",
            OODLE_COMPRESSORS[index_of(&OODLE_COMPRESSORS, p.oodle_compressor)]
                .0
                .into(),
        ),
        (
            "OodleLevel",
            OODLE_LEVELS[index_of(&OODLE_LEVELS, p.oodle_level)]
                .0
                .into(),
        ),
        ("WriteEmptyPak", b(p.write_empty_pak)),
        ("WarnPakNeeded", b(p.warn_pak_needed)),
        ("Compact", b(p.compact)),
        ("AutoCompactUnder", format_size(p.auto_compact_under)),
        ("AskMountPoint", b(p.ask_mount_point)),
        ("MountPointHistory", p.mount_point_history.to_string()),
        (
            "ValidateOnReplace",
            match p.validate_on_replace {
                ValidateOnReplace::Warn => "Warn",
                ValidateOnReplace::Fix => "Fix",
                ValidateOnReplace::Error => "Error",
                ValidateOnReplace::Off => "Off",
            }
            .into(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{parse_options, set_value_text};

    fn same(a: &PackOptions, b: &PackOptions) -> bool {
        format!("{a:?}") == format!("{b:?}")
    }

    #[test]
    fn defaults_round_trip_through_the_form() {
        let d = PackOptions::default();
        let f = PackForm::from_options(&d);
        assert_eq!(f.auto_compact_under, "10M");
        assert_eq!(f.mount_point_history, "10");
        assert!(f.ask_mount_point);
        assert!(same(&f.to_options().unwrap(), &d));
    }

    #[test]
    fn every_choice_survives_the_ini() {
        let shipped = include_str!("../uetoc.ini");
        for mode in 0..ADD_VALIDATIONS.len() {
            for compression in 0..COMPRESSIONS.len() {
                let form = PackForm {
                    validate_add: mode,
                    compression,
                    zlib_level: (compression + 3) % 10,
                    oodle_compressor: compression % OODLE_COMPRESSORS.len(),
                    oodle_level: (compression * 2) % OODLE_LEVELS.len(),
                    compact: mode == 1,
                    auto_compact_under: ["0", "512K", "10M", "2G", "12345"][compression % 5].into(),
                    write_empty_pak: compression % 2 == 0,
                    warn_pak_needed: mode == 0,
                    ask_mount_point: mode != 1,
                    mount_point_history: ["0", "10", "50"][compression % 3].into(),
                    validate: compression % VALIDATIONS.len(),
                };
                let opts = form.to_options().unwrap();
                let mut text = shipped.to_string();
                for (k, v) in ini_values(&opts) {
                    text = set_value_text(&text, "Pack", k, &v);
                }
                let back = parse_options(&text);
                assert!(back.warnings.is_empty(), "{:?}", back.warnings);
                assert!(same(&back.pack, &opts), "{:?}\n{:?}", back.pack, opts);
                let again = PackForm::from_options(&back.pack);
                assert!(same(&again.to_options().unwrap(), &opts));
            }
        }
    }

    #[test]
    fn writing_keeps_comments_and_other_sections() {
        let shipped = include_str!("../uetoc.ini");
        let opts = PackOptions {
            compression: PackCompression::Oodle,
            ..PackOptions::default()
        };
        let mut text = shipped.to_string();
        for (k, v) in ini_values(&opts) {
            text = set_value_text(&text, "Pack", k, &v);
        }
        assert!(
            text.contains("\r\nCompression=Oodle           ; Match"),
            "{text}"
        );
        // a value that reaches the comment column: one space before the comment
        let text = set_value_text(&text, "Pack", "RecentMountPoints", "../../../Game/Content/Mods/WithAVeryLongName/");
        assert!(
            text.contains("\r\nRecentMountPoints=../../../Game/Content/Mods/WithAVeryLongName/ ; "),
            "{text}"
        );
        // a shorter value again: the comment returns to the file's usual column
        let t = set_value_text(&text, "Pack", "RecentMountPoints", "Auto");
        assert!(t.contains("\r\nRecentMountPoints=Auto      ; written by the plugin"), "{t}");
        assert!(text.contains("\r\nValidateOnAdd=Warn          ; new files"), "{text}");
        assert_eq!(text.matches("[Pack]").count(), 1);
        assert_eq!(text.matches("[Options]").count(), 1);
        assert_eq!(text.lines().count(), shipped.lines().count());
        // a missing section is appended
        let t = set_value_text("[Options]\r\nDebugLog=1\r\n", "Pack", "Compact", "1");
        assert_eq!(t, "[Options]\r\nDebugLog=1\r\n\r\n[Pack]\r\nCompact=1\r\n");
        // a missing key goes into its own section, not the first one
        let t = set_value_text(
            "[Options]\r\nA=1\r\n\r\n[Pack]\r\nB=2\r\n",
            "Pack",
            "C",
            "3",
        );
        assert_eq!(t, "[Options]\r\nA=1\r\n\r\n[Pack]\r\nB=2\r\nC=3\r\n");
    }

    #[test]
    fn bad_input_is_explained() {
        let mut f = PackForm::from_options(&PackOptions::default());
        f.auto_compact_under = "ten".into();
        assert!(f.to_options().unwrap_err().contains("not a size"));
        f.auto_compact_under = "0".into();
        f.mount_point_history = "99".into();
        assert!(f.to_options().unwrap_err().contains("0-50"));
        f.mount_point_history = " 3 ".into();
        assert_eq!(f.to_options().unwrap().mount_point_history, 3);
        assert!(normalize_mount_point("C:/Game").unwrap_err().contains("not a mount point"));
        assert_eq!(normalize_mount_point(" auto ").unwrap(), None);
        f.compression = 99;
        assert!(f.to_options().is_err());
    }

    #[test]
    fn level_lists_follow_the_compression() {
        let mut f = PackForm::from_options(&PackOptions::default());
        for (i, (_, c)) in COMPRESSIONS.iter().enumerate() {
            f.compression = i;
            assert_eq!(
                f.zlib_level_used(),
                matches!(
                    c,
                    PackCompression::Zlib | PackCompression::Gzip | PackCompression::Match
                )
            );
            assert_eq!(
                f.oodle_used(),
                matches!(c, PackCompression::Oodle | PackCompression::Match)
            );
        }
    }

    #[test]
    fn sizes_format_as_the_ini_writes_them() {
        assert_eq!(format_size(0), "0");
        assert_eq!(format_size(10 << 20), "10M");
        assert_eq!(format_size(3 << 30), "3G");
        assert_eq!(format_size(1536), "1536");
        assert_eq!(format_size(512 << 10), "512K");
    }
}
