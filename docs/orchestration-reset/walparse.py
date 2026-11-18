# -*- coding: utf-8 -*-
"""Summarize a SQLite WAL: frames, commits (transactions), distinct pages, and
which b-tree each page belongs to (via dbstat on the main db, best effort).

usage: python3 walparse.py <db> <wal>
"""
import collections
import sqlite3
import struct
import sys

db, wal = sys.argv[1:3]
data = open(wal, "rb").read()
page_size = struct.unpack(">I", data[8:12])[0]
off, frames, commits, pages = 32, 0, 0, []
while off + 24 + page_size <= len(data):
    pgno, after = struct.unpack(">II", data[off:off + 8])
    pages.append(pgno)
    frames += 1
    if after:
        commits += 1
    off += 24 + page_size
owner = {}
try:
    c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    for name, pageno in c.execute("select name, pageno from dbstat"):
        owner[pageno] = name
except sqlite3.Error as e:
    print("dbstat unavailable:", e)
by = collections.Counter(owner.get(p, "?new/freelist") for p in pages)
print(f"frames {frames}  commits {commits}  distinct pages {len(set(pages))}")
for name, n in by.most_common(20):
    print(f"  {n:4} frames  {name}")
