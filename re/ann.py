import pickle,bisect,re,sys
ins=pickle.load(open('out/insns.pkl','rb'));A=[a for a,_,_ in ins]
data=open(r'C:\Program Files (x86)\Napster\napster.exe','rb').read()
def cstr(va):
    off=va-0x400000
    if not(0x8c000<=off<len(data)): return None
    e=data.find(b'\0',off); s=data[off:e]
    return s.decode('latin1') if s and all(32<=c<127 or c in(10,13) for c in s) else None
start=int(sys.argv[1],16); n=int(sys.argv[2]); only=len(sys.argv)>3
i=bisect.bisect_left(A,start)
for a,m,o in ins[i:i+n]:
    ann=''
    for mt in re.finditer(r'0x4[89a-c][0-9a-f]{4}',o):
        s=cstr(int(mt.group(),16))
        if s is not None: ann+=' ; '+repr(s)
    if not only or ann or m=='call' or (m=='push' and re.match(r'^(0x)?[0-9a-f]+$',o)) or m=='cmp':
        print(hex(a),m,o,ann)
