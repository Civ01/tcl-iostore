#!/usr/bin/env python3
"""Make small test copies of IoStore containers (.utoc/.ucas) for upload.

The .utoc is copied unchanged. Each .ucas (and _sN partition) keeps its full size,
but only the data of selected chunks is kept; everything else is zeroed, so the
result compresses to almost nothing with 7-Zip. Offsets stay valid, so a reader
sees the real container: full listing, real header and real directory index.
Only the kept chunks can be extracted; the others decode as garbage, so tests use
the manifest (<name>.trim.json) to know which chunks are real.

No AES key is needed: encrypted blocks are copied as they are.

Kept, in this order, until the budget is used up:
  1. the container header and global metadata chunks (ScriptObjects, LoaderGlobal*)
  2. the smallest chunk of every other chunk type in the container
  3. one large chunk (the largest that fits in half of what is left), for multi-block tests
  4. as many of the remaining chunks as fit, smallest first
global.utoc/.ucas are always kept whole (they are small).

Python 3.9+, standard library only.

Usage:
  python ucas_trim.py OUT_DIR CONTAINER.utoc [CONTAINER.utoc ...] [--budget-mb 25]
  python ucas_trim.py OUT_DIR D:\\Game\\Content\\Paks\\*.utoc --budget-mb 25

The budget is the total size of kept (still compressed) chunk data over all given
containers, global excluded. Then compress OUT_DIR with 7-Zip (zeros cost nothing).
"""

import argparse
import glob
import hashlib
import json
import os
import shutil
import struct
import sys

MAGIC = b"-==--==--==--==-"
HEADER_SIZE = 0x90
MAX_KNOWN_VERSION = 8  # ReplaceIoChunkHashWithIoHash (UE 5.5-5.8)
COMPANION_COPY_LIMIT = 1 << 20  # copy the .pak/.sig next to a container only if <= 1 MiB

# Chunk type names. TOC version <= 4 (UE4) and >= 5 (UE5) use different numbering.
TYPES_UE4 = ["Invalid", "InstallManifest", "ExportBundleData", "BulkData", "OptionalBulkData",
             "MemoryMappedBulkData", "LoaderGlobalMeta", "LoaderInitialLoadMeta",
             "LoaderGlobalNames", "LoaderGlobalNameHashes", "ContainerHeader",
             "ShaderCodeLibrary", "ShaderCode"]
TYPES_UE5 = ["Invalid", "ExportBundleData", "BulkData", "OptionalBulkData",
             "MemoryMappedBulkData", "ScriptObjects", "ContainerHeader", "ExternalFile",
             "ShaderCodeLibrary", "ShaderCode", "PackageStoreEntry", "DerivedData",
             "EditorDerivedData", "PackageResource"]
ALWAYS_KEEP = {"ContainerHeader", "ScriptObjects", "LoaderGlobalMeta", "LoaderInitialLoadMeta",
               "LoaderGlobalNames", "LoaderGlobalNameHashes"}


class TrimError(Exception):
    pass


def align16(n):
    return (n + 15) & ~15


def parse_toc(path):
    with open(path, "rb") as f:
        d = f.read()
    if len(d) < HEADER_SIZE or d[:16] != MAGIC:
        raise TrimError("%s: not a vanilla .utoc (bad magic)" % path)
    version = d[16]
    if version == 0 or version > MAX_KNOWN_VERSION:
        raise TrimError("%s: TOC version %d not supported by this script" % (path, version))
    (hdr_size, entries, blocks, block_entry_size, meth_count, meth_len, block_size,
     dir_size, part_count) = struct.unpack_from("<9I", d, 20)
    container_id = struct.unpack_from("<Q", d, 56)[0]
    flags = d[80]
    seeds, part_size, no_phash = struct.unpack_from("<IQI", d, 84)
    if hdr_size != HEADER_SIZE or block_entry_size != 12:
        raise TrimError("%s: unexpected header size %#x / block entry size %d"
                        % (path, hdr_size, block_entry_size))
    if version < 3:  # PartitionSize added in v3
        part_count, part_size = 1, (1 << 64) - 1
    if part_count == 0:
        part_count = 1
    if part_size == 0:
        part_size = (1 << 64) - 1
    if version < 4:
        seeds = 0
    if version < 5:
        no_phash = 0

    o = HEADER_SIZE
    chunk_ids = [d[o + i * 12:o + i * 12 + 12] for i in range(entries)]
    o += entries * 12
    offlen = []
    for i in range(entries):
        e = d[o + i * 10:o + i * 10 + 10]
        offlen.append((int.from_bytes(e[0:5], "big"), int.from_bytes(e[5:10], "big")))
    o += entries * 10
    o += seeds * 4 + no_phash * 4
    blks = []
    for i in range(blocks):
        e = d[o + i * 12:o + i * 12 + 12]
        blks.append((int.from_bytes(e[0:5], "little"), int.from_bytes(e[5:8], "little"),
                     int.from_bytes(e[8:11], "little"), e[11]))
    o += blocks * 12
    if o > len(d):
        raise TrimError("%s: truncated TOC" % path)

    names = TYPES_UE5 if version >= 5 else TYPES_UE4
    chunks = []
    for i, (cid, (off, ln)) in enumerate(zip(chunk_ids, offlen)):
        t = cid[11]
        tname = names[t] if t < len(names) else "Type%d" % t
        if ln == 0:
            rng = []
        else:
            first, last = off // block_size, (off + ln - 1) // block_size
            if last >= len(blks):
                raise TrimError("%s: chunk %d points past the block table" % (path, i))
            rng = list(range(first, last + 1))
        packed = sum(align16(blks[b][1]) for b in rng)
        chunks.append({"index": i, "id": cid.hex(), "type": tname, "size": ln,
                       "packed": packed, "blocks": rng})
    return {"version": version, "container_id": container_id, "flags": flags,
            "block_size": block_size, "part_count": part_count, "part_size": part_size,
            "blocks": blks, "chunks": chunks, "sha1": hashlib.sha1(d).hexdigest()}


def partition_paths(utoc, count):
    base = utoc[:-5]
    return [base + ".ucas" if i == 0 else "%s_s%d.ucas" % (base, i) for i in range(count)]


def select(toc, budget):
    chunks = toc["chunks"]
    keep, used = set(), 0

    def take(c):
        nonlocal used
        keep.add(c["index"])
        used += c["packed"]

    for c in chunks:
        if c["type"] in ALWAYS_KEEP:
            take(c)
    smallest = {}
    for c in chunks:
        if c["index"] in keep or c["size"] == 0:
            continue
        s = smallest.get(c["type"])
        if s is None or c["packed"] < s["packed"]:
            smallest[c["type"]] = c
    for c in sorted(smallest.values(), key=lambda c: c["packed"]):
        if used + c["packed"] <= budget:
            take(c)
    rest = [c for c in chunks if c["index"] not in keep]
    big = [c for c in rest if c["packed"] <= (budget - used) // 2 and len(c["blocks"]) > 1]
    if big:
        take(max(big, key=lambda c: c["packed"]))
    for c in sorted((c for c in chunks if c["index"] not in keep), key=lambda c: c["packed"]):
        if used + c["packed"] > budget:
            break
        take(c)
    return keep, used


def write_trimmed(utoc, toc, keep, out_dir):
    name = os.path.basename(utoc)
    shutil.copy2(utoc, os.path.join(out_dir, name))
    srcs = partition_paths(utoc, toc["part_count"])
    dsts = [os.path.join(out_dir, os.path.basename(p)) for p in srcs]
    ranges = [[] for _ in srcs]
    for i in keep:
        for b in toc["chunks"][i]["blocks"]:
            off, csize = toc["blocks"][b][0], toc["blocks"][b][1]
            part = off // toc["part_size"]
            ranges[part].append((off % toc["part_size"], align16(csize)))
    for src, dst, rs in zip(srcs, dsts, ranges):
        if not os.path.exists(src):
            raise TrimError("%s: missing partition file" % src)
        size = os.path.getsize(src)
        with open(src, "rb") as fi, open(dst, "wb") as fo:
            fo.truncate(size)  # zero-filled (sparse where the file system allows)
            for off, ln in sorted(set(rs)):
                ln = min(ln, size - off)
                fi.seek(off)
                fo.seek(off)
                fo.write(fi.read(ln))
        shutil.copystat(src, dst)
    base = utoc[:-5]
    for ext in (".pak", ".sig"):
        p = base + ext
        if os.path.exists(p) and os.path.getsize(p) <= COMPANION_COPY_LIMIT:
            shutil.copy2(p, os.path.join(out_dir, os.path.basename(p)))
    return srcs


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("out_dir")
    ap.add_argument("utoc", nargs="+")
    ap.add_argument("--budget-mb", type=float, default=25.0,
                    help="total kept chunk data over all containers, global excluded (default 25)")
    args = ap.parse_args()

    paths = []
    for pattern in args.utoc:
        found = glob.glob(pattern) or [pattern]
        paths.extend(p for p in found if p.lower().endswith(".utoc"))
    paths = sorted(set(os.path.abspath(p) for p in paths))
    if not paths:
        sys.exit("no .utoc files given")
    os.makedirs(args.out_dir, exist_ok=True)

    budget = int(args.budget_mb * 1024 * 1024)
    tocs = {}
    for p in paths:
        try:
            tocs[p] = parse_toc(p)
        except (TrimError, OSError) as e:
            print("skipped: %s" % e)
    regular = [p for p in tocs if os.path.basename(p).lower() != "global.utoc"]
    per_container = budget // max(1, len(regular))

    for p, toc in tocs.items():
        is_global = os.path.basename(p).lower() == "global.utoc"
        try:
            if is_global:
                keep, used = {c["index"] for c in toc["chunks"]}, sum(c["packed"] for c in toc["chunks"])
            else:
                keep, used = select(toc, per_container)
            srcs = write_trimmed(p, toc, keep, args.out_dir)
        except (TrimError, OSError) as e:
            print("skipped: %s" % e)
            continue
        manifest = {
            "format": 1,
            "source_utoc_sha1": toc["sha1"],
            "toc_version": toc["version"],
            "container_id": str(toc["container_id"]),
            "partitions": [os.path.basename(s) for s in srcs],
            "chunk_count": len(toc["chunks"]),
            "kept_all": len(keep) == len(toc["chunks"]),
            "kept": [{"index": c["index"], "id": c["id"], "type": c["type"], "size": c["size"]}
                     for c in toc["chunks"] if c["index"] in keep],
        }
        mpath = os.path.join(args.out_dir, os.path.basename(p)[:-5] + ".trim.json")
        with open(mpath, "w", encoding="utf-8") as f:
            json.dump(manifest, f, indent=1)
        print("%-50s v%d  kept %d/%d chunks, %.1f MiB of data"
              % (os.path.basename(p), toc["version"], len(keep), len(toc["chunks"]), used / 1048576))


if __name__ == "__main__":
    main()
