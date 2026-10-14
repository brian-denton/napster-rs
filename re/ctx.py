import sys,pickle,bisect
ins=pickle.load(open(sys.argv[1],'rb')); addrs=[a for a,_,_ in ins]
before=int(sys.argv[3]) if len(sys.argv)>3 else 25; after=int(sys.argv[4]) if len(sys.argv)>4 else 15
for t in sys.argv[2].split(','):
    v=int(t,16)+0x400000 if int(t,16)<0x400000 else int(t,16)
    hits=[k for k,(a,m,o) in enumerate(ins) if hex(v) in o]
    print("=== target",hex(v),"hits",len(hits))
    for k in hits:
        print("--")
        for a,m,o in ins[max(0,k-before):k+after]: print(hex(a),m,o, '<<<' if hex(v) in o else '')
