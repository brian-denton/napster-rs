import sys, pickle

ins = pickle.load(open(sys.argv[1], 'rb'))
data = open(sys.argv[2], 'rb').read()
target = sys.argv[3]


def cstr(va):
    off = va - 0x400000
    if not (0 <= off < len(data)):
        return None
    e = data.find(b'\0', off)
    s = data[off:e]
    if all(32 <= c < 127 for c in s):
        return s.decode()
    return None


for k, (a, m, o) in enumerate(ins):
    if m == 'call' and o == target:
        pushes = []
        for j in range(k - 1, max(0, k - 40), -1):
            aa, mm, oo = ins[j]
            if mm == 'push':
                pushes.append(oo)
            if mm in ('call', 'ret', 'jmp'):
                break
            if len(pushes) >= 2:
                break
        t = pushes[0] if pushes else '?'
        f = pushes[1] if len(pushes) > 1 else '?'
        fs = None
        if f.startswith('0x'):
            fs = cstr(int(f, 16))
        try:
            tv = int(t, 16) if t.startswith('0x') else int(t)
        except Exception:
            tv = t
        print(hex(a), tv, repr(fs) if fs is not None else f)
