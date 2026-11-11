"""把 OpenCC 的字典转成紧凑的二进制块，供 Rust 端 include_bytes! 使用。

格式（小端）：
  magic "ZHC1"
  u32 条目数
  逐条：u16 key_len, key utf8, u16 val_len, val utf8
键按字节序排序，Rust 端可以二分查找，也可以一次性建 HashMap。

用法：uv run --with opencc-python-reimplemented python native/scripts/gen_zhconv.py
"""

import os
import struct

import opencc

OUT = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
    "crates",
    "core",
    "assets",
    "zhconv.bin",
)


def load_pairs(path, first_only=True):
    pairs = {}
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.rstrip("\n")
            if not line or line.startswith("#"):
                continue
            parts = line.split("\t")
            if len(parts) < 2:
                continue
            key = parts[0]
            vals = parts[1].split(" ")
            if first_only:
                val = vals[0]
            else:
                val = vals[0]
            if not key or not val or key == val:
                continue
            pairs[key] = val
    return pairs


def main():
    dic = os.path.join(os.path.dirname(opencc.__file__), "dictionary")
    # 简化：s2t 与 t2s 各一组，key 加前缀 'S' / 'T' 区分方向
    entries = {}
    for tag, fname in [
        ("S", "STCharacters.txt"),
        ("S", "STPhrases.txt"),
        ("T", "TSCharacters.txt"),
        ("T", "TSPhrases.txt"),
    ]:
        p = os.path.join(dic, fname)
        if not os.path.exists(p):
            print("跳过（缺文件）", fname)
            continue
        for k, v in load_pairs(p).items():
            entries[(tag, k)] = v
    print("条目数", len(entries))
    items = sorted(entries.items(), key=lambda kv: (kv[0][0], kv[0][1]))
    blob = bytearray(b"ZHC1")
    blob += struct.pack("<I", len(items))
    for (tag, k), v in items:
        kb = (tag + k).encode("utf-8")
        vb = v.encode("utf-8")
        blob += struct.pack("<H", len(kb)) + kb
        blob += struct.pack("<H", len(vb)) + vb
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    with open(OUT, "wb") as f:
        f.write(blob)
    print("写出", OUT, len(blob), "字节")


if __name__ == "__main__":
    main()
