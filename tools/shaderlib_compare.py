#!/usr/bin/env python3
"""Compare two legacy shader libraries (.ushaderbytecode) by content.

Shader map hashes, which shaders each shader map holds (by hash), and every shader's
decompressed code. Order, compression and preload entries may differ.
Usage: shaderlib_compare.py A.ushaderbytecode B.ushaderbytecode [--oodle liboo2corelinux64.so.9]
Version 2 (20-byte hashes, UE <= 5.7) and 3 (8-byte hashes, UE 5.8). Python 3.9+, stdlib only.
"""
import argparse, ctypes, struct, sys


class R:
    def __init__(self, d): self.d, self.p = d, 0
    def take(self, n):
        b = self.d[self.p:self.p + n]
        if len(b) != n: raise ValueError("truncated")
        self.p += n; return b
    def u32(self): return struct.unpack("<I", self.take(4))[0]
    def arr(self, size, fmt=None):
        n = self.u32()
        return [self.take(size) if fmt is None else struct.unpack(fmt, self.take(size)) for _ in range(n)]


def parse(path):
    d = open(path, "rb").read()
    r = R(d)
    ver = r.u32()
    h = {2: 20, 3: 8}[ver]
    smh = r.arr(h); sh = r.arr(h)
    sme = r.arr(16, "<IIII")
    se = r.arr(17, "<QIIB")
    pre = r.arr(16, "<qq")
    idx = [x[0] for x in r.arr(4, "<I")]
    code = d[r.p:]
    return dict(ver=ver, smh=smh, sh=sh, sme=sme, se=se, idx=idx, code=code)


def load_oodle(path):
    if not path: return None
    lib = ctypes.CDLL(path)
    f = lib.OodleLZ_Decompress
    f.restype = ctypes.c_int64
    f.argtypes = [ctypes.c_void_p, ctypes.c_int64, ctypes.c_void_p, ctypes.c_int64] + [ctypes.c_int] * 3 + [ctypes.c_void_p, ctypes.c_int64, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_int64, ctypes.c_int]
    return f


def shader_code(lib, i, oodle):
    off, size, usize, freq = lib["se"][i]
    raw = lib["code"][off:off + size]
    if size == usize: return raw
    if oodle is None: return ("compressed", raw)
    out = ctypes.create_string_buffer(usize)
    n = oodle(raw, size, out, usize, 1, 0, 0, None, 0, None, None, None, 0, 3)
    if n != usize: raise ValueError(f"oodle failed on shader {i}")
    return out.raw


def content(lib, oodle):
    shaders = {lib["sh"][i]: (lib["se"][i][3], shader_code(lib, i, oodle)) for i in range(len(lib["sh"]))}
    maps = {}
    for m, (off, n, _, _) in zip(lib["smh"], lib["sme"]):
        maps[m] = sorted(lib["sh"][lib["idx"][off + k]] for k in range(n))
    return shaders, maps


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("a"); ap.add_argument("b"); ap.add_argument("--oodle")
    a = ap.parse_args()
    oodle = load_oodle(a.oodle)
    A, B = parse(a.a), parse(a.b)
    print(f"version {A['ver']} / {B['ver']}; shader maps {len(A['smh'])} / {len(B['smh'])}; shaders {len(A['sh'])} / {len(B['sh'])}")
    sa, ma = content(A, oodle); sb, mb = content(B, oodle)
    bad = 0
    if set(ma) != set(mb):
        print(f"shader map sets differ: {len(set(ma) - set(mb))} only in A, {len(set(mb) - set(ma))} only in B"); bad += 1
    for m in set(ma) & set(mb):
        if ma[m] != mb[m]:
            print(f"shader map {m.hex()} holds different shaders"); bad += 1
    if set(sa) != set(sb):
        print(f"shader sets differ: {len(set(sa) - set(sb))} only in A, {len(set(sb) - set(sa))} only in B"); bad += 1
    diff = [s for s in set(sa) & set(sb) if sa[s] != sb[s]]
    if diff:
        print(f"{len(diff)} shaders differ in frequency or code"); bad += 1
    print("SAME CONTENT" if bad == 0 else "DIFFERENT")
    return 0 if bad == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
