import sys, pefile, capstone, re
pe = pefile.PE(sys.argv[1])
base = pe.OPTIONAL_HEADER.ImageBase
text = [s for s in pe.sections if s.Name.startswith(b'.text')][0]
code = text.get_data(); tva = base + text.VirtualAddress
md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_32); md.skipdata=True
insns = list(md.disasm(code, tva))
print("insns", len(insns), file=sys.stderr)
import pickle
pickle.dump([(i.address, i.mnemonic, i.op_str) for i in insns], open(sys.argv[2],'wb'))
