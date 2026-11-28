#!/usr/bin/env python3
"""An asciicast with markers -> a contact sheet of the marked screens, or
(with --gif) an animated GIF of the whole recording.

    render.py CAST OUT.png COLUMNS [FIRST [UNDER.png]]
    render.py CAST OUT.gif --gif [FPS]

FIRST numbers the first shot; UNDER adds the shots under a sheet made
before (same width). Needs Pillow and the DejaVu fonts. See README.md."""
import json, re, sys
from PIL import Image, ImageDraw, ImageFont
CAST, OUT = sys.argv[1], sys.argv[2]
GIF = len(sys.argv) > 3 and sys.argv[3] == "--gif"
FPS = float(sys.argv[4]) if GIF and len(sys.argv) > 4 else 2.0
COLN = 1 if GIF else int(sys.argv[3])
FIRST = int(sys.argv[4]) if not GIF and len(sys.argv) > 4 else 1      # number of the first shot
UNDER = sys.argv[5] if not GIF and len(sys.argv) > 5 else None         # an existing sheet to add them under
lines = open(CAST).read().splitlines(); head = json.loads(lines[0]); W, H = head["width"], head["height"]
events = [json.loads(l) for l in lines[1:]]
BASE = [(0x1d,0x1f,0x21),(0xcc,0x66,0x66),(0xb5,0xbd,0x68),(0xf0,0xc6,0x74),(0x81,0xa2,0xbe),(0xb2,0x94,0xbb),(0x8a,0xbe,0xb7),(0xc5,0xc8,0xc6),
        (0x66,0x66,0x66),(0xd5,0x4e,0x53),(0xb9,0xca,0x4a),(0xe7,0xc5,0x47),(0x7a,0xa6,0xda),(0xc3,0x97,0xd8),(0x70,0xc0,0xb1),(0xea,0xea,0xea)]
PAL = BASE[:] + [tuple(0 if v == 0 else 55 + v*40 for v in (r,g,b)) for r in range(6) for g in range(6) for b in range(6)] + [(8+i*10,)*3 for i in range(24)]
FG0, BG0 = (0xc5,0xc8,0xc6), (0x15,0x17,0x19)
class Term:
    def __init__(s): s.c=[[(" ",FG0,BG0,False,False) for _ in range(W)] for _ in range(H)]; s.x=s.y=0; s.fg,s.bg,s.b,s.rev,s.ul=FG0,BG0,False,False,False; s.buf=""
    def sgr(s,p):
        ps=[int(v) if v.isdigit() else 0 for v in p.split(";")] if p else [0]; i=0
        while i<len(ps):
            v=ps[i]
            if v==0: s.fg,s.bg,s.b,s.rev,s.ul=FG0,BG0,False,False,False
            elif v==1: s.b=True
            elif v==22: s.b=False
            elif v==4: s.ul=True
            elif v==24: s.ul=False
            elif v==7: s.rev=True
            elif v==27: s.rev=False
            elif 30<=v<=37: s.fg=PAL[v-30]
            elif 90<=v<=97: s.fg=PAL[v-82]
            elif 40<=v<=47: s.bg=PAL[v-40]
            elif 100<=v<=107: s.bg=PAL[v-92]
            elif v==39: s.fg=FG0
            elif v==49: s.bg=BG0
            elif v in (38,48,58) and i+1<len(ps):
                col=None
                if ps[i+1]==5 and i+2<len(ps): col=PAL[ps[i+2]%256]; i+=2
                elif ps[i+1]==2 and i+4<len(ps): col=tuple(ps[i+2:i+5]); i+=4
                if col and v==38: s.fg=col
                elif col and v==48: s.bg=col
            i+=1
    def put(s,ch):
        if s.x>=W: s.x,s.y=0,min(s.y+1,H-1)
        f,b=(s.bg,s.fg) if s.rev else (s.fg,s.bg)
        s.c[s.y][s.x]=(ch,f,b,s.b,s.ul); s.x+=1
    def feed(s,data):
        t=s.buf+data; s.buf=""; i=0
        while i<len(t):
            ch=t[i]
            if ch=="\x1b":
                m=re.match(r"\x1b\][^\x07\x1b]*(\x07|\x1b\\)",t[i:])
                if m: i+=len(m.group(0)); continue
                m=re.match(r"\x1b\[([?0-9;:]*)([@-~])",t[i:])
                if not m:
                    if len(t)-i<24: s.buf=t[i:]; return
                    i+=2; continue
                p,cmd=m.group(1),m.group(2)
                if cmd in "Hf":
                    q=[int(v) if v else 1 for v in p.split(";")] if p else [1,1]; s.y=min(max(q[0]-1,0),H-1); s.x=min(max((q[1] if len(q)>1 else 1)-1,0),W-1)
                elif cmd=="m" and not p.startswith("?"): s.sgr(p.replace(":",";"))
                elif cmd=="J" and p in ("2","3"): s.c=[[(" ",FG0,BG0,False,False) for _ in range(W)] for _ in range(H)]
                elif cmd=="K":
                    for x in range(s.x,W): s.c[s.y][x]=(" ",FG0,s.bg,False,False)
                elif cmd=="G": s.x=min(max(int(p or 1)-1,0),W-1)
                elif cmd=="C": s.x=min(s.x+int(p or 1),W-1)
                i+=len(m.group(0))
            elif ch=="\r": s.x=0; i+=1
            elif ch=="\n": s.y=min(s.y+1,H-1); i+=1
            elif ch<" ": i+=1
            else: s.put(ch); i+=1
font=ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",15); bold=ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf",15)
cw=round(font.getlength("M")); chh=19; PAD=8
def draw(cells):
    img=Image.new("RGB",(W*cw+2*PAD,H*chh+2*PAD),BG0); d=ImageDraw.Draw(img)
    for y,row in enumerate(cells):
        for x,(ch,f,b,bo,ul) in enumerate(row):
            px,py=PAD+x*cw,PAD+y*chh
            if b!=BG0: d.rectangle([px,py,px+cw-1,py+chh-1],fill=b)
            if ch!=" ": d.text((px,py+1),ch,font=bold if bo else font,fill=f)
            if ul: d.line([px,py+chh-2,px+cw-1,py+chh-2],fill=f)
    return img
if GIF:
    # a frame every 1/FPS seconds of the recording, each shown that long
    t=Term(); frames=[]; step=1.0/FPS; next_at=0.0
    for at,kind,data in events:
        while at>=next_at:
            frames.append(draw([r[:] for r in t.c])); next_at+=step
        if kind=="o": t.feed(data)
    frames.append(draw([r[:] for r in t.c]))
    small=[f.convert("P",palette=Image.Palette.ADAPTIVE,colors=128) for f in frames]
    small[0].save(OUT,save_all=True,append_images=small[1:],duration=int(step*1000),loop=0,optimize=True)
    print(len(frames),"frames",frames[0].size); sys.exit(0)
t=Term(); shots=[]
for at,kind,data in events:
    if kind=="o": t.feed(data)
    elif kind=="m": shots.append((data,draw([r[:] for r in t.c])))
fw,fh=shots[0][1].size; CAP=26; G=12; rows=-(-len(shots)//COLN)
sheet=Image.new("RGB",(COLN*fw+(COLN+1)*G,rows*(fh+CAP)+(rows+1)*G),(40,42,46)); d=ImageDraw.Draw(sheet); capf=ImageFont.truetype("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",17)
for i,(note,img) in enumerate(shots):
    x=G+(i%COLN)*(fw+G); y=G+(i//COLN)*(fh+CAP+G)
    d.text((x+2,y+3),f"{i+FIRST}. {note}  ({W}x{H})",font=capf,fill=(230,230,230)); sheet.paste(img,(x,y+CAP))
if UNDER:
    old=Image.open(UNDER).convert("RGB")
    assert old.width==sheet.width, (old.width, sheet.width)
    # the old sheet's last gap is the new one's first
    both=Image.new("RGB",(old.width,old.height+sheet.height-G),(40,42,46)); both.paste(old,(0,0)); both.paste(sheet,(0,old.height-G)); sheet=both
sheet.save(OUT); print(len(shots),"shots",sheet.size)
