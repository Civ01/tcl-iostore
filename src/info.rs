//! `<ContainerId>.txt`: the container described as fixed-width text for a monospace font
//! (SPEC §5).

use std::fmt::Write as _;
use std::path::Path;

use iostore::{ChunkMeta, Container, container_header_version_name};

use crate::global::GlobalInfo;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Thousands separators for sizes: 3697719812 -> 3,697,719,812.
fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Width in characters (what a monospace font shows).
fn width(s: &str) -> usize {
    s.chars().count()
}

/// Sections of key/value lines; all keys padded to the same width.
#[derive(Default)]
struct Doc {
    parts: Vec<Part>,
}

enum Part {
    Section(&'static str),
    Kv(String, String),
    Raw(String),
}

impl Doc {
    fn section(&mut self, name: &'static str) {
        self.parts.push(Part::Section(name));
    }
    fn kv(&mut self, k: &str, v: impl Into<String>) {
        self.parts.push(Part::Kv(k.to_string(), v.into()));
    }
    fn raw(&mut self, line: String) {
        self.parts.push(Part::Raw(line));
    }
    fn render(self) -> String {
        let kw = self.parts.iter().filter_map(|p| if let Part::Kv(k, _) = p { Some(width(k)) } else { None }).max().unwrap_or(0);
        let mut s = String::new();
        let mut first = true;
        for p in self.parts {
            match p {
                Part::Section(n) => {
                    if !first {
                        s.push('\n');
                    }
                    let _ = writeln!(s, "[{n}]");
                }
                Part::Kv(k, v) => {
                    let pad = kw - width(&k);
                    // multi-line values continue under the value column
                    let mut lines = v.lines();
                    let _ = writeln!(s, "{k}{}  {}", " ".repeat(pad), lines.next().unwrap_or(""));
                    for l in lines {
                        let _ = writeln!(s, "{}  {l}", " ".repeat(kw));
                    }
                }
                Part::Raw(l) => {
                    let _ = writeln!(s, "{l}");
                }
            }
            first = false;
        }
        s
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Align {
    Left,
    Right,
}

/// A table padded to the widest cell per column; the last column isn't padded.
fn table(header: &[(&str, Align)], rows: &[Vec<String>]) -> Vec<String> {
    let mut w: Vec<usize> = header.iter().map(|(h, _)| width(h)).collect();
    for r in rows {
        for (i, c) in r.iter().enumerate() {
            w[i] = w[i].max(width(c));
        }
    }
    let line = |cells: &[&str]| {
        let mut out = String::new();
        let n = header.len();
        for (i, c) in cells.iter().enumerate() {
            let pad = w[i] - width(c);
            let last = i + 1 == n;
            match header[i].1 {
                Align::Right => {
                    out.push_str(&" ".repeat(pad));
                    out.push_str(c);
                }
                Align::Left => {
                    out.push_str(c);
                    if !last {
                        out.push_str(&" ".repeat(pad));
                    }
                }
            }
            if !last {
                out.push_str("  ");
            }
        }
        out
    };
    let mut out = Vec::with_capacity(rows.len() + 2);
    out.push(line(&header.iter().map(|(h, _)| *h).collect::<Vec<_>>()));
    // the last column isn't padded, so its dashes only cover the header (paths can be long)
    let dashes: Vec<String> =
        w.iter().enumerate().map(|(i, &n)| "-".repeat(if i + 1 == w.len() { width(header[i].0) } else { n })).collect();
    out.push(line(&dashes.iter().map(String::as_str).collect::<Vec<_>>()));
    for r in rows {
        out.push(line(&r.iter().map(String::as_str).collect::<Vec<_>>()));
    }
    out
}

/// Renders the info file. `index_error` is set when the directory index couldn't be
/// read (e.g. encrypted and no key): paths then read "(encrypted)" / "(unavailable)".
pub fn render(c: &Container, global: Option<&GlobalInfo>, index_error: Option<&str>) -> String {
    let h = c.header();
    let mut d = Doc::default();
    let utoc = c.utoc_path();
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let size = |p: &Path| std::fs::metadata(p).map(|m| format!("{} bytes", group(m.len()))).unwrap_or_else(|_| "missing".into());

    d.section("Container");
    d.kv("Folder", utoc.parent().map(|p| p.display().to_string()).unwrap_or_default());
    d.kv("TOC file", format!("{}  ({})", name(utoc), size(utoc)));
    for (i, p) in c.partitions().iter().enumerate() {
        let label = if c.partitions().len() > 1 { format!("Data file {i}") } else { "Data file".into() };
        d.kv(&label, format!("{}  ({})", name(&p.path), p.size.map(|n| format!("{} bytes", group(n))).unwrap_or_else(|| "missing".into())));
    }
    let pak = utoc.with_extension("pak");
    d.kv("Companion .pak", if pak.is_file() { format!("{}  ({})", name(&pak), size(&pak)) } else { "none".into() });
    match global {
        Some(g) if g.utoc == utoc => d.kv("Global container", format!("this container (TOC version {})", g.version.0)),
        Some(g) => d.kv(
            "Global container",
            format!(
                "{}\nTOC version {}{}",
                g.utoc.display(),
                g.version.0,
                if g.version != h.version { " (differs from this container)" } else { "" }
            ),
        ),
        None => d.kv("Global container", "not found"),
    }
    d.kv("Game profile", c.profile().id());

    d.section("Header");
    d.kv("Magic", String::from_utf8_lossy(&h.magic).into_owned());
    d.kv("Version", format!("{} {} (UE {})", h.version.0, h.version.name(), h.version.engines()));
    d.kv("Header size", format!("{:#x}", h.header_size));
    d.kv("Entries", group(h.entry_count as u64));
    d.kv("Compression blocks", format!("{} (entry size {})", group(h.block_count as u64), h.block_entry_size));
    d.kv("Compression block size", format!("{} ({:#x})", group(h.block_size as u64), h.block_size));
    let methods = c.toc().compression_methods.join(", ");
    d.kv(
        "Compression methods",
        format!("{} (name length {})", if methods.is_empty() { "none".into() } else { methods }, h.compression_method_name_length),
    );
    d.kv("Directory index size", group(h.directory_index_size as u64));
    d.kv("Partitions", format!("{} (stored {})", h.effective_partition_count(), h.partition_count));
    d.kv(
        "Partition size",
        if h.effective_partition_size() == u64::MAX { "unlimited".to_string() } else { group(h.effective_partition_size()) },
    );
    d.kv("Container id", format!("{} ({:#018x})", h.container_id, h.container_id.0));
    d.kv("Encryption key GUID", format!("{}{}", h.key_guid, if h.key_guid.is_zero() { " (main key)" } else { "" }));
    d.kv("Flags", format!("{} ({:#04x})", h.flags, h.flags.0));
    d.kv("Perfect hash seeds", group(h.perfect_hash_seed_count as u64));
    d.kv("Chunks without perfect hash", group(h.chunks_without_perfect_hash_count as u64));
    d.kv("Chunk hash", h.version.hash_algorithm().name());

    d.section("ContainerHeader");
    match c.container_header() {
        None => d.kv("Container header", "none (no ContainerHeader chunk)"),
        Some(Ok(ch)) => {
            d.kv("Version", format!("{} {}", ch.version, container_header_version_name(ch.version)));
            d.kv("Container id", ch.container_id.to_string());
            d.kv("Packages", group(ch.package_count as u64));
            if let Some(n) = ch.optional_segment_package_count {
                d.kv("Optional segment packages", group(n as u64));
            }
        }
        Some(Err(e)) => d.kv("Container header", format!("can't be read: {e}")),
    }

    d.section("Signatures");
    match &c.toc().signatures {
        None => d.kv("Signed", "no"),
        Some(sig) => {
            d.kv("Signed", "yes (not verified: needs the game's public key)");
            d.kv("Signature size", sig.hash_size.to_string());
            d.kv("Block hashes", group(sig.block_hashes.len() as u64));
        }
    }

    // path per TOC entry
    let index = c.loaded_directory_index();
    let mut paths: Vec<Option<&str>> = vec![None; c.chunk_count() as usize];
    if let Some(idx) = index {
        for f in &idx.files {
            if let Some(slot) = paths.get_mut(f.toc_index as usize) {
                *slot = Some(f.path.as_str());
            }
        }
    }
    d.section("Files");
    if let Some(idx) = index {
        d.kv("Mount point", idx.mount_point.clone());
        d.kv("Named files", group(idx.files.len() as u64));
    } else if let Some(e) = index_error {
        d.kv("Directory index", e.to_string());
    }
    d.kv("Chunks", group(c.chunk_count() as u64));
    d.raw(String::new());

    let unavailable = if c.index_needs_key() && index.is_none() { "(encrypted)" } else { "(unavailable)" };
    let psize = h.effective_partition_size();
    let mut order: Vec<u32> = Vec::with_capacity(c.chunk_count() as usize);
    if let Some(idx) = index {
        order.extend(idx.files.iter().map(|f| f.toc_index).filter(|&i| i < c.chunk_count()));
    }
    let mut listed = vec![false; c.chunk_count() as usize];
    for &i in &order {
        listed[i as usize] = true;
    }
    order.extend((0..c.chunk_count()).filter(|&i| !listed[i as usize]));
    // Two narrow tables instead of one wide one (Lister wraps long lines): what a file is,
    // then where its chunk lies. Both list the same rows, linked by "#".
    let mut files = Vec::with_capacity(order.len());
    let mut chunks = Vec::with_capacity(order.len());
    for i in order {
        let Ok(info) = c.chunk(i) else { continue };
        let path = match paths[i as usize] {
            Some(p) => p.to_string(),
            None if index.is_some() || c.toc().directory_index.is_empty() => "(no path)".to_string(),
            None => unavailable.to_string(),
        };
        let (part, off) = match c.toc().blocks.get(info.blocks.start as usize).filter(|_| !info.blocks.is_empty()) {
            Some(b) if psize != u64::MAX => ((b.offset / psize).to_string(), group(b.offset % psize)),
            Some(b) => ("0".into(), group(b.offset)),
            None => ("-".into(), "-".into()),
        };
        let mut flags = Vec::new();
        if info.meta.flags & ChunkMeta::COMPRESSED != 0 {
            flags.push("Compressed");
        }
        if info.meta.flags & ChunkMeta::MEMORY_MAPPED != 0 {
            flags.push("MemoryMapped");
        }
        files.push(vec![i.to_string(), info.kind.name(), group(info.size), group(info.packed_size), c.chunk_methods(&info).join("+"), path]);
        chunks.push(vec![
            i.to_string(),
            info.id.hex(),
            part,
            off,
            info.blocks.len().to_string(),
            if flags.is_empty() { "-".to_string() } else { flags.join(",") },
            if info.has_hash() { hex(&info.meta.hash) } else { "-".into() },
        ]);
    }
    use Align::*;
    let files_header = [("#", Right), ("Type", Left), ("Size", Right), ("Packed", Right), ("Compression", Left), ("Path", Left)];
    for l in table(&files_header, &files) {
        d.raw(l);
    }
    d.section("Chunks");
    d.raw(format!("Same rows as [Files], by \"#\" (the TOC entry index). Offset is in the data file; hash: {}.", h.version.hash_algorithm().name()));
    d.raw(String::new());
    let chunks_header = [("#", Right), ("Chunk id", Left), ("Part", Right), ("Offset", Right), ("Blocks", Right), ("Flags", Left), ("Hash", Left)];
    for l in table(&chunks_header, &chunks) {
        d.raw(l);
    }

    // UTF-8 BOM + CRLF for Windows tools
    let text = d.render();
    let mut out = String::with_capacity(text.len() + text.len() / 40 + 3);
    out.push('\u{feff}');
    for line in text.lines() {
        out.push_str(line.trim_end());
        out.push_str("\r\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_and_tables() {
        assert_eq!(group(0), "0");
        assert_eq!(group(999), "999");
        assert_eq!(group(1000), "1,000");
        assert_eq!(group(3697719812), "3,697,719,812");
        let t = table(
            &[("#", Align::Right), ("Name", Align::Left), ("Path", Align::Left)],
            &[vec!["1".into(), "a".into(), "x/y".into()], vec!["10".into(), "long".into(), "z".into()]],
        );
        assert_eq!(t, [" #  Name  Path", "--  ----  ----", " 1  a     x/y", "10  long  z"]);
        let t = table(&[("N", Align::Left), ("Path", Align::Left)], &[vec!["a".into(), "a/very/long/path".into()]]);
        assert_eq!(t[1], "-  ----");
        let mut d = Doc::default();
        d.section("S");
        d.kv("A", "1");
        d.kv("Longer", "2\n3");
        assert_eq!(d.render(), "[S]\nA       1\nLonger  2\n        3\n");
    }
}
