from common import *

# Momentaufnahme: Sektion voice_3 (4/6), Durchlauf 1, Takt 7 Schlag 1. Release wurde gedrückt,
# Wechsel zu „alles" am Loop-Ende, also in 2 Takten. keys nimmt auf (überschriebener Zustand),
# voice nimmt Ebene 3 als Overdub auf.
TILES = [
    # name, color, inp, is-class, icon, word, sub, soll-state, soll-word, layers(list), on-button, levels
    ("gitarre", "var(--trk-1)", "In 2", "play", "play", "Wiedergabe", "Loop 8 Takte · 10,2 s", "play", "Wiedergabe", ["on"], "play", (0, 58, 66)),
    ("voice", "var(--trk-2)", "In 1", "dub", "dub", "Overdub", "Ebene 3 · Takt 7 / 8", "play", "Wiedergabe", ["on", "on", "rec"], "dub", (72, 70, 78)),
    ("chor", "var(--trk-3)", "In 1", "stop", "stop", "Still", "kein Loop", "hear", "Mithören", [], None, (72, 0, 0)),
    ("perc", "var(--trk-4)", "In 3", "play", "play", "Wiedergabe", "Loop 8 Takte · 10,2 s", "play", "Wiedergabe", ["on"], "play", (0, 61, 70)),
    ("keys", "var(--trk-5)", "In 5–6", "rec", "rec", "Aufnahme", "Takt 7 / 8 · stereo", "play", "Wiedergabe", ["rec"], "rec", (66, 0, 0)),
]
# MIDI-Belegungen (für Learn-Ansicht): Element-Schlüssel -> Abzeichen
BIND = {
    "release": "FS 1", "start": "FS 3", "stopall": "FS 4",
    "voice.dub": "Pad 37", "voice.play": "Pad 38", "gitarre.rec": "Pad 40", "gitarre.play": "Pad 41",
    "keys.rec": "Pad 44", "chor.hear": "CC 20",
}

def mb(key, learn, target=None):
    if not learn: return ""
    if key == target:
        return '<span class="mb mb-at waitb">wartet …</span>'
    if key in BIND:
        return f'<span class="mb mb-at">{BIND[key]}</span>'
    return ""

def lrn(key, learn, target=None):
    if not learn: return ""
    c = " lrn"
    if key in BIND: c += " bound"
    if key == target: c += " target"
    return c

def body(learn=False, target=None):
    t = target
    tiles = []
    for (n, c, inp, iscls, icn, word, sub, ss, sw, lay, onb, (lin, lout, pk)) in TILES:
        tilecls = " rec" if iscls == "rec" else (" dub" if iscls == "dub" else "")
        lays = "".join(f'<i class="{x}"></i>' for x in lay) + "".join('<i></i>' for _ in range(max(0, 3 - len(lay))))
        btns = ""
        for k, lab, ico in (("rec", "Aufnahme", "rec"), ("dub", "Overdub", "dub"), ("play", "Wiedergabe", "play"), ("stop", "Stopp", "stop"), ("hear", "Mithören", "hear")):
            on = (k == onb) or (k == "hear" and n in ("voice", "chor", "keys"))
            key = f"{n}.{k}"
            btns += f'<span class="tb {k}{" on" if on else ""}{lrn(key, learn, t)}">{ic(ico)}<span>{lab}</span>{mb(key, learn, t)}</span>'
        tiles.append(f'''<div class="tile{tilecls}" style="--c:{c}">
  <div class="tn"><b>{n}</b><span>{inp}</span><span class="lay" title="Ebenen">{lays}</span></div>
  <div class="is {iscls}">{ic(icn)}<div class="w"><b>{word}</b><span>{sub}</span></div></div>
  <div class="soll"><span class="lbl">danach</span>{ic("arr-r")}<span class="st {ss}">{ic(ss)}{sw}</span></div>
  <div class="tm"><span>Ein</span><span class="hm"><i style="--l:{lin}%;--p:{lin+6 if lin else 0}%"></i></span><span>Aus</span><span class="hm"><i style="--l:{lout}%;--p:{pk}%"></i></span></div>
  <div class="tbtns">{btns}</div>
</div>''')
    blocks = []
    for i, (sid, s, n, auto, q) in enumerate(SECS):
        cls = "blk"
        extra = ""
        if i < 3: cls += " past"
        if i == 3: cls += " now"
        if i == 4: cls += " next"
        meta = f'{n} T · '
        if i < 3: meta = f'{ic("check")}{n} T'
        elif auto: meta += f'{ic("auto")}auto'
        else: meta += f'<span class="rel">{ic("hand")}</span>'
        if i == 3: meta = f'{n} T · <span class="hide-s">Durchlauf 1 · </span><span class="rel">{ic("loop")}bis Release</span>'
        if i == 4: meta = f'{n} T · <span class="rel">{ic("hand")}Release</span>'
        tag = '<span class="tag">jetzt</span><span class="prog" style="--p:79%"></span>' if i == 3 else ('<span class="tag">als Nächstes</span>' if i == 4 else "")
        gl = f' {lrn("goto"+str(i), learn, t)}' if learn else ""
        blocks.append(f'<div class="{cls}{gl}" style="--n:{max(n, 2.5)}"><div class="bn"><i>{i+1}</i><b>{sid}</b></div><div class="bm">{meta}</div>{tag}</div>')
    return f'''<div class="live">
  <div class="lv-top">
    <section class="panel pos">
      <div class="kick"><span class="k">Sektion <b>4</b> / 6</span><span class="lv-chip run">{ic("play")}läuft · Durchlauf 1</span><span class="lv-chip loop">{ic("loop")}loopt bis Release</span></div>
      <div class="big">
        <div class="num-block"><label>Takt</label><div class="hero">7<small>/ 8</small></div></div>
        <div class="right">
          <h1>voice_3</h1>
          <div class="num-block"><label>Schlag</label><div class="bigbeats"><i class="one on">1</i><i>2</i><i>3</i></div></div>
        </div>
      </div>
      <div><div class="secbar"><i class="done"></i><i class="done"></i><i class="done"></i><i class="done"></i><i class="done"></i><i class="done"></i><i class="now"></i><i class="tail"></i></div>
      <div class="secbar-l"><span>141 BPM · 3/4 · Klick an</span><span>Takt 31 von 41 im Arrangement</span></div></div>
    </section>
    <div class="ctlcol">
      <section class="release armed{lrn("release", learn, t)}">
        <div class="rh"><span class="lbl">{ic("release")}Release</span><div class="grow"></div>{('<span class="mb" style="height:24px;font-size:14px;padding:0 9px">FS 1</span>' if learn else '')}</div>
        <div class="state">{ic("armed")}Wechsel armiert</div>
        <div class="to"><span>nach</span>alles</div>
        <div class="when">am Loop-Ende · in <b>2 Takten</b></div>
        <div class="countdown"><i class="cur"></i><i></i></div>
      </section>
      <div class="lv-row">
        <span class="bigbtn running{lrn("start", learn, t)}">{ic("start")}Läuft <span class="sub">seit 1:13</span>{mb("start", learn)}</span>
        <span class="bigbtn{lrn("stopall", learn, t)}">{ic("stopall")}Alles stoppen{mb("stopall", learn)}</span>
      </div>
    </div>
  </div>
  <section class="panel lane-live">{"".join(blocks)}</section>
  <div class="tiles">{"".join(tiles)}</div>
</div>'''

LOCK = '<span class="pill" title="Im Live-Modus ist das Arrangement gesperrt">' + ic("lock", "--ic:11px") + 'Arrangement gesperrt</span>'

def page(title, comment, learn=False, target=None, side="", learnbar=""):
    st = LOCK + '<span class="pill"><i class="led"></i>48 kHz · 128 · 0 Aussetzer</span>'
    main = '<div class="live-wrap">' + body(learn, target) + '</div>'
    if side:
        main = f'<div class="main learn" style="min-height:0">{main}{side}</div>'
    inner = f'<div style="display:grid;grid-template-rows:{"auto " if learnbar else ""}1fr;min-height:0">{learnbar}{main}</div>'
    return head(title, ("live.css",)) + f'''<body{' class="learn-on"' if learn else ""}>
<!-- {comment} -->
<div class="window">
{titlebar(mode="live", compact=True, learn=learn, status=st)}
{inner}
</div>
</body></html>
'''

if __name__ == "__main__":
    open(D + "live.html", "w").write(page("Henrys Looper · Live", "Mockup 3: Live-Modus im Lauf. voice_3 loopt (Durchlauf 1, Takt 7), keys nimmt auf, voice nimmt Ebene 3 als Overdub auf; Release ist gedrückt, der Wechsel zu „alles“ ist für das Loop-Ende armiert. Responsiv: 1440×900 und 1024×768."))
