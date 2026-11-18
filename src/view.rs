//! Mount point → listing prefix (SPEC §4), same split as tcl-uepak: a mount point
//! splits into a hidden base (leading `../`, `./`, `/`) and a visible part shown as
//! folders in `Folder` view.

/// `../../../TheIsle/Content/` → base `../../../`, visible `[TheIsle, Content]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Mount {
    pub base: String,
    pub visible: Vec<String>,
    /// The original ended with `/` (kept when rebuilding).
    pub trailing_slash: bool,
}

impl Mount {
    pub fn parse(mount: &str) -> Self {
        let norm = mount.replace('\\', "/");
        let mut base = String::new();
        let mut rest = norm.as_str();
        loop {
            if let Some(r) = rest.strip_prefix("../") {
                base.push_str("../");
                rest = r;
            } else if let Some(r) = rest.strip_prefix("./") {
                base.push_str("./");
                rest = r;
            } else if let Some(r) = rest.strip_prefix('/') {
                base.push('/');
                rest = r;
            } else {
                break;
            }
        }
        let visible = rest
            .split('/')
            .filter(|s| !s.is_empty() && *s != "." && *s != "..")
            .map(String::from)
            .collect();
        Mount {
            base,
            visible,
            trailing_slash: norm.ends_with('/'),
        }
    }

    /// Mount point string with the visible part cut to its first `k` segments.
    pub fn with_visible_len(&self, k: usize) -> String {
        let mut s = self.base.clone();
        let vis = self.visible[..k].join("/");
        s.push_str(&vis);
        if !vis.is_empty() && self.trailing_slash {
            s.push('/');
        }
        s
    }

    /// `TheIsle\Content` (Folder view prefix).
    pub fn display_prefix(&self) -> String {
        self.visible.join("\\")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mounts() {
        let m = Mount::parse("../../../Moadra/Content/Mods/DebugInfo/");
        assert_eq!(m.base, "../../../");
        assert_eq!(m.display_prefix(), "Moadra\\Content\\Mods\\DebugInfo");
        assert!(Mount::parse("../../../").visible.is_empty());
        assert_eq!(Mount::parse("/Game/").display_prefix(), "Game");
    }
}
