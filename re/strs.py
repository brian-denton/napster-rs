import re,sys
d=open(sys.argv[1],'rb').read()
for m in re.finditer(rb'[\x20-\x7e]{5,}',d):
    print(hex(m.start()), m.group().decode())
for m in re.finditer(rb'(?:[\x20-\x7e]\x00){5,}',d):
    print(hex(m.start()),'W', m.group().decode('utf-16le'))
