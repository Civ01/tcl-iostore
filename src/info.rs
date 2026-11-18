//! `<ContainerId>.txt`: the container described as text (SPEC §5).

use std::fmt::Write as _;
use std::path::Path;

use iostore::{ChunkMeta, Container, container_header_version_name};

use crate::global::GlobalInfo;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Renders the info file. `index_error` is set when the directory index couldn't be
/// read (e.g. encrypted and no key): paths then read "(encrypted)" / "(unavailable)".
pub fn render(c: &Container, global: Option<&GlobalInfo>, index_error: Option<&str>) -> String {
    let h = c.header();
    let mut s = String::new();
    let utoc = c.utoc_path();
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let size = |p: &Path| std::fs::metadata(p).map(|m| m.len().to_string()).unwrap_or_else(|_| "missing".into());

    let _ = writeln!(s, "[Container]");
    let _ = writeln!(s, "Folder\t{}", utoc.parent().map(|p| p.display().to_string()).unwrap_or_default());
    let _ = writeln!(s, "TOC file\t{}\t{} bytes", name(utoc), size(utoc));
    for p in c.partitions() {
        let _ = writeln!(s, "Data file\t{}\t{}", name(&p.path), p.size.map(|n| format!("{n} bytes")).unwrap_or_else(|| "missing".into()));
    }
    let pak = utoc.with_extension("pak");
    let _ = writeln!(s, "Companion .pak\t{}", if pak.is_file() { format!("{} ({} bytes)", name(&pak), size(&pak)) } else { "none".into() });
    match global {
        Some(g) => {
            let same = g.utoc == utoc;
            let _ = writeln!(
                s,
                "Global container\t{}\tTOC version {}{}",
                if same { "this container".to_string() } else { g.utoc.display().to_string() },
                g.version.0,
                if g.version != h.version { " (differs from this container)" } else { "" }
            );
        }
        None => {
            let _ = writeln!(s, "Global container\tnot found");
        }
    }
    let _ = writeln!(s, "Game profile\t{}", c.profile().id());

    let _ = writeln!(s, "\n[Header]");
    let _ = writeln!(s, "Magic\t{}", String::from_utf8_lossy(&h.magic));
    let _ = writeln!(s, "Version\t{} {} (UE {})", h.version.0, h.version.name(), h.version.engines());
    let _ = writeln!(s, "Header size\t{:#x}", h.header_size);
    let _ = writeln!(s, "Entries\t{}", h.entry_count);
    let _ = writeln!(s, "Compression blocks\t{} (entry size {})", h.block_count, h.block_entry_size);
    let _ = writeln!(s, "Compression block size\t{} ({:#x})", h.block_size, h.block_size);
    let methods = c.toc().compression_methods.join(", ");
    let _ = writeln!(
        s,
        "Compression methods\t{}\t(name length {})",
        if methods.is_empty() { "none".into() } else { methods },
        h.compression_method_name_length
    );
    let _ = writeln!(s, "Directory index size\t{}", h.directory_index_size);
    let _ = writeln!(s, "Partitions\t{} (stored {})", h.effective_partition_count(), h.partition_count);
    let _ = writeln!(
        s,
        "Partition size\t{}",
        if h.effective_partition_size() == u64::MAX { "unlimited".to_string() } else { h.effective_partition_size().to_string() }
    );
    let _ = writeln!(s, "Container id\t{}\t({:#018x})", h.container_id, h.container_id.0);
    let _ = writeln!(s, "Encryption key GUID\t{}{}", h.key_guid, if h.key_guid.is_zero() { " (main key)" } else { "" });
    let _ = writeln!(s, "Flags\t{}\t({:#04x})", h.flags, h.flags.0);
    let _ = writeln!(s, "Perfect hash seeds\t{}", h.perfect_hash_seed_count);
    let _ = writeln!(s, "Chunks without perfect hash\t{}", h.chunks_without_perfect_hash_count);
    let _ = writeln!(s, "Chunk hash\t{}", h.version.hash_algorithm().name());

    let _ = writeln!(s, "\n[ContainerHeader]");
    match c.container_header() {
        None => {
            let _ = writeln!(s, "none (no ContainerHeader chunk)");
        }
        Some(Ok(ch)) => {
            let _ = writeln!(s, "Version\t{} {}", ch.version, container_header_version_name(ch.version));
            let _ = writeln!(s, "Container id\t{}", ch.container_id);
            let _ = writeln!(s, "Packages\t{}", ch.package_count);
            if let Some(n) = ch.optional_segment_package_count {
                let _ = writeln!(s, "Optional segment packages\t{n}");
            }
        }
        Some(Err(e)) => {
            let _ = writeln!(s, "can't be read: {e}");
        }
    }

    let _ = writeln!(s, "\n[Signatures]");
    match &c.toc().signatures {
        None => {
            let _ = writeln!(s, "Signed\tno");
        }
        Some(sig) => {
            let _ = writeln!(s, "Signed\tyes (not verified: needs the game's public key)");
            let _ = writeln!(s, "Signature size\t{}", sig.hash_size);
            let _ = writeln!(s, "Block hashes\t{}", sig.block_hashes.len());
        }
    }

    // path per TOC entry
    let mut paths: Vec<Option<&str>> = vec![None; c.chunk_count() as usize];
    let index = c.loaded_directory_index();
    if let Some(idx) = index {
        for f in &idx.files {
            if let Some(slot) = paths.get_mut(f.toc_index as usize) {
                *slot = Some(f.path.as_str());
            }
        }
    }
    let _ = writeln!(s, "\n[Files]");
    if let Some(idx) = index {
        let _ = writeln!(s, "Mount point\t{}", idx.mount_point);
        let _ = writeln!(s, "Named files\t{}", idx.files.len());
    } else if let Some(e) = index_error {
        let _ = writeln!(s, "Directory index\t{e}");
    }
    let _ = writeln!(s, "Path\tChunk id\tType\tSize\tPacked\tPartition\tOffset\tBlocks\tCompression\tFlags\tHash");
    let unavailable = if c.index_needs_key() && index.is_none() { "(encrypted)" } else { "(unavailable)" };
    let psize = c.header().effective_partition_size();
    let mut order: Vec<u32> = Vec::with_capacity(c.chunk_count() as usize);
    if let Some(idx) = index {
        order.extend(idx.files.iter().map(|f| f.toc_index).filter(|&i| i < c.chunk_count()));
    }
    let mut listed = vec![false; c.chunk_count() as usize];
    for &i in &order {
        listed[i as usize] = true;
    }
    order.extend((0..c.chunk_count()).filter(|&i| !listed[i as usize]));
    for i in order {
        let Ok(info) = c.chunk(i) else { continue };
        let path = match paths[i as usize] {
            Some(p) => p.to_string(),
            None if index.is_some() || c.toc().directory_index.is_empty() => "(no path)".to_string(),
            None => unavailable.to_string(),
        };
        let (part, off) = match c.toc().blocks.get(info.blocks.start as usize).filter(|_| !info.blocks.is_empty()) {
            Some(b) if psize != u64::MAX => ((b.offset / psize).to_string(), (b.offset % psize).to_string()),
            Some(b) => ("0".into(), b.offset.to_string()),
            None => ("-".into(), "-".into()),
        };
        let mut flags = Vec::new();
        if info.meta.flags & ChunkMeta::COMPRESSED != 0 {
            flags.push("Compressed");
        }
        if info.meta.flags & ChunkMeta::MEMORY_MAPPED != 0 {
            flags.push("MemoryMapped");
        }
        let _ = writeln!(
            s,
            "{path}\t{}\t{}\t{}\t{}\t{part}\t{off}\t{}\t{}\t{}\t{}",
            info.id.hex(),
            info.kind.name(),
            info.size,
            info.packed_size,
            info.blocks.len(),
            c.chunk_methods(&info).join("+"),
            if flags.is_empty() { "-".to_string() } else { flags.join(",") },
            if info.has_hash() { hex(&info.meta.hash) } else { "-".into() },
        );
    }
    // UTF-8 BOM + CRLF for Windows tools
    let mut out = String::with_capacity(s.len() + s.len() / 40 + 3);
    out.push('\u{feff}');
    for line in s.lines() {
        out.push_str(line);
        out.push_str("\r\n");
    }
    out
}
