from arr import *

POS = 18.33  # Takt 19, Schlag 2
def W(kind, extra=""):
    return f'<div class="wave {kind}"{extra}></div>'

def lanes():
    L = []
    g = [clip("rec", 0, 8, hd_right='<span class="lay">8 T</span>', body=W("gtr"))]
    for s in (8, 16, 24, 32):
        g.append(clip("play", s, 8, body=W("gtr"), cls=" inh" if s == 24 else ""))
    g.append(clip("stop", 40, 1))
    v = [clip("hear", 0, 8), clip("rec", 8, 8, hd_right='<span class="lay">Ebene 1</span>', body=W("vox")),
         clip("dub", 16, 8, hd_right='<span class="lay">Ebene 2</span>', body=W("vox", ' style="--pw:calc(2.33 * var(--b))"').replace('wave vox', 'wave vox part')),
         clip("dub", 24, 8, hd_right='<span class="lay">Ebene 3</span>', cls=" inh"),
         clip("play", 32, 8, hd_right='<span class="lay">3 Ebenen</span>'), clip("stop", 40, 1)]
    c = [clip("stop", 0, 8), clip("stop", 8, 8), clip("stop", 16, 8), clip("stop", 24, 8, cls=" inh"),
         clip("hear", 32, 8, hd_right='<span class="lay">live</span>'), clip("stop", 40, 1)]
    p = [clip("rec", 0, 8, hd_right='<span class="lay">8 T</span>', body=W("perc"))]
    for s in (8, 16, 24, 32):
        p.append(clip("play", s, 8, body=W("perc"), cls=" inh" if s == 24 else ""))
    p.append(clip("stop", 40, 1))
    k = [clip("stop", 0, 8), clip("stop", 8, 8), clip("hear", 16, 8), clip("rec", 24, 8, hd_right='<span class="lay">8 T</span>'),
         clip("play", 32, 8), clip("stop", 40, 1)]
    rows = [g, v, c, p, k]
    out = ['<div class="lanes">', '<div class="secline" style="--s:8"></div><div class="secline" style="--s:16"></div><div class="secline" style="--s:24"></div><div class="secline" style="--s:32"></div><div class="secline" style="--s:40"></div>',
           '<div class="endzone" style="--s:41"></div>', '<div class="donezone" style="--pos:%s"></div>' % POS]
    for i, r in enumerate(rows):
        out.append(f'<div class="lane{" alt" if i % 2 else ""}{" sel" if i == 1 else ""}">' + "".join(r) + "</div>")
    out.append(f'<div class="playhead" style="--pos:{POS}"></div></div>')
    return "\n".join(out)

RUNTIME = [("play", "Wiedergabe"), ("dub", "Overdub"), ("empty", "leer"), ("play", "Wiedergabe"), ("hear", "mithören")]
METER_H = [(62, 70), (71, 78), (0, 0), (55, 74), (40, 52)]

def heads():
    out = []
    for i, (n, c, inp, ch, pre, let) in enumerate(TRACKS):
        stc, stl = RUNTIME[i]
        icn = {"play": "play", "dub": "dub", "empty": "empty", "hear": "hear"}[stc]
        l, p = METER_H[i]
        m2 = f'<i style="--l:{l-4}%;--p:{p-3}%"></i>' if ch == "stereo" else ""
        out.append(f'''<div class="th c{" sel" if i == 1 else ""}" style="--c:{c}"><div class="clr"></div><div class="body">
  <div class="name"><span class="idx">{i+1}</span><b>{n}</b><span class="ch">{inp}</span><div class="grow"></div><span class="st {stc}">{ic(icn)}{stl}</span></div>
  <div class="meterline"><span class="hm"><i style="--l:{l}%;--p:{p}%"></i>{m2}</span><span class="chip hear sq{" on" if n in ("voice", "chor", "keys") else ""}" style="height:18px">{ic("hear")}</span></div>
</div></div>''')
    out.append('<div class="addtrack" style="height:34px"><span class="btn s">' + ic("plus") + 'Spur hinzufügen</span></div>')
    return '<div class="heads">' + "\n".join(out) + "</div>"

def fpos(db):
    pts = [(6, 100), (0, 82), (-6, 66), (-12, 50), (-24, 30), (-48, 10), (-90, 0)]
    for (d1, p1), (d2, p2) in zip(pts, pts[1:]):
        if d2 <= db <= d1:
            return p2 + (p1 - p2) * (db - d2) / (d1 - d2)
    return 0

SCALE = '<div class="scale">' + "".join(f'<span style="bottom:calc({fpos(d)}% * (100% - 8px) / 100% + 4px);top:auto;transform:translateY(50%)">{t}</span>' for d, t in [(6, "+6"), (0, "0"), (-6, "6"), (-12, "12"), (-24, "24"), (-48, "48")]) + '<span style="bottom:4px;transform:translateY(50%)">∞</span></div>'

def strip(i, sel=False):
    n, c, inp, ch, pre, let = TRACKS[i]
    stc, stl = RUNTIME[i]
    icn = {"play": "play", "dub": "dub", "empty": "empty", "hear": "hear"}[stc]
    l, p = METER_H[i]
    meters = f'<i style="--l:{l}%;--p:{p}%"></i>' + (f'<i style="--l:{l-4}%;--p:{p-3}%"></i>' if ch == "stereo" else "")
    db = [-3.5, 0.0, -8.0, -6.5, -10.0][i]
    pan = [-0.2, 0, 0.25, 0.0, 0][i]
    panl = ["L 20", "M", "R 25", "M", "M"][i]
    plug = '<div class="ins plug full"><i class="led"></i>Plate-Hall<span class="v">AU</span></div>' if n == "voice" else '<div class="ins plug">' + ic("plus") + 'Plugin</div>'
    hearon = n in ("voice", "chor", "keys")
    sends = f'''<div class="sends"><span></span><span class="hd">Saal</span><span class="hd">Kopfh.</span>
      <span>Loop</span><span class="sw on">{ic("speaker")}</span><span class="sw on">{ic("phones")}</span>
      <span>Eingang</span><span class="sw{" on h" if False else ""}">{ic("speaker")}</span><span class="sw{" on h" if hearon else ""}">{ic("phones")}</span></div>'''
    return f'''<div class="strip{" sel" if sel else ""}">
  <div class="zone"><span class="caps">Inserts <span class="dim2">{pre}</span></span>
    <div style="display:flex;justify-content:space-between">{fxl(let)}<span class="btn s sq ghost" style="height:16px;width:16px">{ic("power","--ic:11px")}</span></div>
    {plug}<div class="ins plug">{ic("plus")}Plugin</div></div>
  <div class="zone"><span class="caps">Busse</span>{sends}</div>
  <div class="panrow"><span class="knob" style="--v:{pan}"></span><span class="v">{panl}</span></div>
  <div class="fz">{SCALE}<div class="pair"><div class="fader"><div class="cap" style="--f:calc({fpos(db)}% * (100% - 8px) / 100% + 4px)"></div></div><div class="vm{" w" if ch == "mono" else ""}">{meters}</div></div></div>
  <div class="stripfoot">
    <div class="vol"><b>{de(db)}</b><span class="pk">{["−9,2", "−4,8", "−∞", "−7,1", "−13,5"][i]} dB</span></div>
    <div class="nm" style="--c:{c}"><span>{n}</span><div class="grow"></div><span class="st {stc}" style="height:16px;padding:0 4px">{ic(icn)}</span></div>
  </div>
</div>'''

def fmt(s):
    return s

def bus(name, icon, out, db, lv, click=False, plugin=None):
    plug = f'<div class="ins plug full"><i class="led"></i>{plugin}<span class="v">AU</span></div>' if plugin else f'<div class="ins plug">{ic("plus")}Plugin</div>'
    clickrow = f'<div class="ins on" style="justify-content:space-between">{ic("metro")}Klick<span class="v">−12 dB</span></div>' if click else '<div class="ins" style="color:var(--tx-4)">kein Klick</div>'
    return f'''<div class="strip bus">
  <div class="zone"><span class="caps">Inserts</span>{plug}<div class="ins plug">{ic("plus")}Plugin</div></div>
  <div class="zone"><span class="caps">Ausgang</span><div class="ins on">{ic("speaker" if icon == "speaker" else "phones")}{out}<span class="v">stereo</span></div>{clickrow}</div>
  <div class="panrow" style="color:var(--tx-3);font-size:10px;height:36px">Stereo-Summe</div>
  <div class="fz">{SCALE}<div class="pair"><div class="fader"><div class="cap" style="--f:calc({fpos(db)}% * (100% - 8px) / 100% + 4px)"></div></div><div class="vm w"><i style="--l:{lv}%;--p:{lv+7}%"></i><i style="--l:{lv-3}%;--p:{lv+4}%"></i></div></div></div>
  <div class="stripfoot">
    <div class="vol"><b>{de(db)}</b><span class="pk">−3,1 dB</span></div>
    <div class="nm" style="--c:var(--tx-2)">{ic(icon)}<span>{name}</span></div>
  </div>
</div>'''

def mixer():
    strips = "".join(strip(i, i == 1) for i in range(5))
    return f'''<section class="mixer">
  <div class="mixer-h"><span class="caps" style="color:var(--tx-2)">{ic("sliders","--ic:12px")} Mixer</span><span class="dim" style="font-size:11px">5 Spuren · 2 Busse</span><div class="grow"></div>
    <div class="seg" style="--ctl-h:20px"><span class="on">Spuren + Busse</span><span>Nur Busse</span></div><span class="btn s sq ghost">{ic("x")}</span></div>
  <div class="strips">{strips}<div class="strip-gap"></div>
  {bus("Main · Saal", "speaker", "Out 1–2", 0.0, 74, plugin="Limiter")}
  {bus("Monitor · Kopfh.", "phones", "Out 3–4", -4.0, 66, click=True)}
  <div class="strip ghoststrip"><span>{ic("plus","--ic:16px")}</span><b>Spur</b><span class="dim2">3 frei</span></div>
  </div>
</section>'''

def inspector():
    eq = '''<svg viewBox="0 0 280 120" preserveAspectRatio="none">
<g stroke="rgba(255,255,255,.06)" stroke-width="1">
<line x1="0" y1="30" x2="280" y2="30"/><line x1="0" y1="60" x2="280" y2="60" stroke="rgba(255,255,255,.12)"/><line x1="0" y1="90" x2="280" y2="90"/>
<line x1="40" y1="0" x2="40" y2="120"/><line x1="93" y1="0" x2="93" y2="120"/><line x1="146" y1="0" x2="146" y2="120"/><line x1="199" y1="0" x2="199" y2="120"/><line x1="252" y1="0" x2="252" y2="120"/></g>
<path d="M0 120 C 20 120, 30 70, 50 64 C 70 60, 80 70, 100 72 C 120 74, 130 62, 160 56 C 190 50, 200 44, 230 40 C 250 38, 265 38, 280 38 L280 120 Z" fill="rgba(142,162,255,.14)"/>
<path d="M0 120 C 20 120, 30 70, 50 64 C 70 60, 80 70, 100 72 C 120 74, 130 62, 160 56 C 190 50, 200 44, 230 40 C 250 38, 265 38, 280 38" fill="none" stroke="#8ea2ff" stroke-width="2"/>
<g fill="#0e1013" stroke="#fff" stroke-width="1.6"><circle cx="38" cy="78" r="5"/><circle cx="100" cy="72" r="5"/><circle cx="200" cy="44" r="5"/></g>
<g fill="#737c8a" font-size="8" font-family="-apple-system"><text x="42" y="116">100</text><text x="149" y="116">1k</text><text x="255" y="116">10k</text></g>
</svg>'''
    return f'''<aside class="insp">
  <div class="insp-h">
    <div class="kicker"><span class="caps">Kanal 2 · Effektkette</span><span class="st dub">{ic("dub")}Overdub · Ebene 2</span></div>
    <h2><i class="c" style="width:4px;height:20px;border-radius:2px;background:var(--trk-2);display:inline-block"></i>voice</h2>
    <div class="sub">In 1 · mono · Latenz 7,3 ms gemessen + 0,0 ms</div>
  </div>
  <div class="fxd">
    <div class="frow"><label>Preset</label><div><span class="field sel" style="flex:1">Stimme{ic("chev-d","")}</span><span class="btn s">Bypass</span></div></div>
    <div class="chain"><span class="on"><b>H</b>Hochp.</span><span class="on sel"><b>E</b>EQ</span><span class="on"><b>K</b>Komp</span><span class="on"><b>D</b>Delay</span><span class="on"><b>R</b>Hall</span><span class="on plug" style="border-color:rgba(142,162,255,.45);background:rgba(142,162,255,.1)"><b style="color:#c9d3ff">P</b>Plugin</span></div>
    <div class="eqgraph">{eq}</div>
    <div class="kn-row">
      <div class="kn"><span class="knob s" style="--v:-0.5"></span><span class="v">120 Hz</span><label>Tief</label></div>
      <div class="kn"><span class="knob s" style="--v:-0.15"></span><span class="v">−2,0 dB</span><label>Mitte</label></div>
      <div class="kn"><span class="knob s" style="--v:0.4"></span><span class="v">+3,5 dB</span><label>Präsenz</label></div>
      <div class="kn"><span class="knob s" style="--v:0.1"></span><span class="v">0,9</span><label>Güte</label></div>
    </div>
  </div>
  <div class="fxd">
    <span class="caps">Kompressor · Delay · Hall</span>
    <div class="gr"><span style="width:58px">Reduktion</span><span class="bar"><i></i></span><span class="num" style="color:var(--tx-1)">−3,2 dB</span></div>
    <div class="frow"><label>Delay <span class="dim num" style="font-size:11px">319 ms</span></label><div><div class="seg" style="--ctl-h:22px"><span>1/4</span><span class="on">1/8.</span><span>1/8</span><span>1/8T</span></div></div></div>
    <div class="frow"><label>Hall-Anteil</label><div><span class="hm" style="flex:1"><i style="--l:28%;--p:0%;background:linear-gradient(to right,var(--accent) 28%,var(--bg-4) 28%)"></i></span><span class="num" style="width:34px;text-align:right">28 %</span></div></div>
  </div>
  <div class="fxd" style="border-bottom:0">
    <span class="caps">Ebenen <span class="dim2" style="text-transform:none;letter-spacing:0;font-weight:500">· 2 von 16</span></span>
    <div class="layer"><span class="no">1</span><span class="hm" style="flex:1"><i style="--l:64%;--p:70%"></i></span><span class="num">0,0 dB</span><span class="btn s sq">{ic("mute")}</span><span class="btn s sq ghost">{ic("x")}</span></div>
    <div class="layer rec"><span class="no">2</span><span class="hm" style="flex:1"><i style="--l:71%;--p:78%"></i></span><span class="st dub" style="height:18px">{ic("dub")}nimmt auf</span></div>
  </div>
</aside>'''

html = head("Henrys Looper · Arrangement mit Mixer") + f'''<body>
<!-- Mockup 2: Arrangement-Modus mit Mixer. Probedurchlauf läuft: Takt 19 in voice_2, voice nimmt
     Ebene 2 auf (Overdub). Wellenformen erscheinen, sobald ein Loop existiert. Rechts die Effektkette
     des ausgewählten Kanals. -->
<div class="window">
{titlebar(running=True, sec="3 / 6", secname="voice_2", bar="3", bars="8", beat=2)}
  <div class="workspace">
    <div class="toolbar">
      <div class="seg"><span class="on">{ic("grid")}Raster</span><span>{ic("code")}Text (YAML)</span></div>
      <div class="sep"></div>
      <span class="btn s">{ic("plus")}Sektion</span><span class="btn s">{ic("dup")}Duplizieren</span><span class="btn s sq">{ic("arr-l")}</span><span class="btn s sq">{ic("arr-r")}</span><span class="btn s danger">{ic("trash")}Löschen</span>
      <div class="sep"></div><span class="btn s">{ic("plus")}Spur</span>
      <div class="grow"></div>
      <span class="lbl">Ansicht folgt Position</span><span class="toggle on"></span>
      <div class="sep"></div>
      <span class="btn s on">{ic("sliders")}Mixer</span><span class="btn s on">{ic("split")}Inspektor</span>
    </div>
    <div class="main">
      <div class="editor with-mixer">
        <div class="arr compact" style="--rowh:50px">
          <div class="corner"><div class="row"><span class="caps">Spuren</span><span class="dim2" style="font-size:10px">5 / 8</span></div><div class="row"><span class="st play">{ic("play")}läuft · Durchlauf 1</span></div></div>
          {ruler(cur=2, pos=POS, stopped=False)}
          {heads()}
          {lanes()}
        </div>
        {mixer()}
      </div>
      {inspector()}
    </div>
    <div class="statusbar"><span>Probedurchlauf · 0:43 / ≈ 0:52</span><span>Nächster Wechsel: <b style="color:var(--tx-1)">voice_3</b> automatisch in 5 Takten</span><div class="grow"></div><span>CPU 7 % · max. Callback 0,61 ms</span><span class="ok">{ic("check")} Partitur gültig</span></div>
  </div>
</div>
</body></html>
'''
open(D + "arrangement-mixer.html", "w").write(html)
