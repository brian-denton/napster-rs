"""Recover case -> handler mapping for an MSVC switch on the message type.

usage: dispatch.py insns.pkl napster.exe <start_va> <end_va>
"""
import sys, pickle, re, struct, bisect

ins = pickle.load(open(sys.argv[1], 'rb'))
data = open(sys.argv[2], 'rb').read()
lo, hi = int(sys.argv[3], 16), int(sys.argv[4], 16)
addrs = [a for a, _, _ in ins]
BASE = 0x400000


def rd(va, n):
    return data[va - BASE: va - BASE + n]


def cstr(va):
    off = va - BASE
    if not (0x8c000 <= off < len(data)):
        return None
    e = data.find(b'\0', off)
    s = data[off:e]
    if len(s) >= 1 and all(32 <= c < 127 or c in (9, 10, 13) for c in s):
        return s.decode()
    return None


body = [(a, m, o) for a, m, o in ins if lo <= a < hi]
cases = {}
for k, (a, m, o) in enumerate(body):
    # direct compare
    if m == 'cmp' and o.startswith('eax, '):
        try:
            v = int(o.split(', ')[1], 16) if '0x' in o else int(o.split(', ')[1])
        except ValueError:
            continue
        for nm, no in ((body[k + 1][1], body[k + 1][2]), (body[k + 2][1], body[k + 2][2])):
            if nm == 'je':
                cases.setdefault(v, int(no, 16))
    # jump table: [add eax, -base | lea ecx,[eax-base]] cmp X ja; mov cl, byte [eax+t1]; jmp [ecx*4+t2]
    if m == 'jmp' and re.match(r'dword ptr \[e.x\*4 \+ 0x', o):
        t2 = int(re.search(r'0x[0-9a-f]+', o).group(), 16)
        win = body[max(0, k - 6):k]
        t1 = None
        bias = 0
        n = None
        for _, mm, oo in win:
            mt = re.match(r'byte ptr \[e.x \+ (0x[0-9a-f]+)\]', oo.split(', ', 1)[-1])
            if mm == 'mov' and mt:
                t1 = int(mt.group(1), 16)
            if mm == 'add' and oo.startswith('eax, '):
                x = int(oo.split(', ')[1], 16)
                bias = -((x ^ 0xffffffff) + 1) & 0xffffffff if x > 0x7fffffff else x
                if x > 0x7fffffff:
                    bias = x - 0x100000000
            if mm == 'add' and oo.startswith('eax, -'):
                bias = -int(oo.split('-')[1], 16)
            if mm == 'lea':
                mt2 = re.search(r'\[e.x - (0x[0-9a-f]+)\]', oo)
                if mt2:
                    bias = -int(mt2.group(1), 16)
            if mm == 'cmp':
                n = int(oo.split(', ')[1], 16)
        if n is None:
            continue
        for i in range(n + 1):
            idx = rd(t1 + i, 1)[0] if t1 else i
            tgt = struct.unpack('<I', rd(t2 + idx * 4, 4))[0]
            cases.setdefault(i - bias, tgt)

# drop default target(s): the most common handler
from collections import Counter
cnt = Counter(cases.values())
default = cnt.most_common(1)[0][0]
hs = sorted(set(cases.values()))
for v in sorted(cases):
    h = cases[v]
    if h == default:
        continue
    # strings & calls in handler until next handler start
    i = bisect.bisect_left(addrs, h)
    nxt = [x for x in hs if x > h]
    end = min(nxt[0] if nxt else h + 0x300, h + 0x600)
    strs, sends = [], []
    while i < len(ins) and ins[i][0] < end:
        _, mm, oo = ins[i]
        for mt in re.finditer(r'0x4[89a-c][0-9a-f]{4}', oo):
            s = cstr(int(mt.group(), 16))
            if s and s not in strs:
                strs.append(s)
        i += 1
    print(f'{v:5d} (0x{v:x}) -> {h:#x}  ' + ' || '.join(repr(s) for s in strs[:6]))
