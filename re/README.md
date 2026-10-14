# Reverse-engineering scripts

These scripts do static analysis of `napster.exe` (v2.0 BETA 10). They only
read the file and never run it. Findings are in `../docs/PROTOCOL.md`.

```bash
uv venv .venv && uv pip install --python .venv capstone pefile
.venv/Scripts/python strs.py  "C:/Program Files (x86)/Napster/napster.exe" > out/napster_strings.txt
.venv/Scripts/python xref.py  "C:/Program Files (x86)/Napster/napster.exe" out/insns.pkl   # disassemble .text
.venv/Scripts/python sends.py out/insns.pkl "C:/Program Files (x86)/Napster/napster.exe" 0x42f120   # every SendMessage(type, fmt)
.venv/Scripts/python dispatch.py out/insns.pkl "C:/Program Files (x86)/Napster/napster.exe" 0x420c50 0x4230e0  # server-message switch
.venv/Scripts/python ann.py 0x410fb0 300 1   # annotated disassembly (strings resolved)
.venv/Scripts/python ctx.py out/insns.pkl 0x49d638 30 25   # code around references to a string
```
