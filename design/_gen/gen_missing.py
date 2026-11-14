import gen_mixer as gm
from arr import *
from gen_learn import brow

def heads():
    out = []
    binds = {"gitarre": ("Pad 40", "Pad 41", None), "voice": ("Pad 37", "Pad 38", None), "chor": (None, None, "CC 20"), "perc": (None, None, None), "keys": ("Pad 44", None, None)}
    for i, (n, c, inp, ch, pre, let) in enumerate(TRACKS):
        r, p, h = binds[n]
        def b(x): return f'<span class="mb mb-b">{x}</span>' if x else ""
        def cl(x): return " lrn bound" if x else " lrn"
        out.append(f'''<div class="th" style="--c:{c}"><div class="clr"></div><div class="body" style="gap:8px;justify-content:center">
  <div class="name"><span class="idx">{i+1}</span><b>{n}</b><span class="ch">{inp} · {ch}</span></div>
  <div class="ctl" style="gap:7px"><span class="chip sq{cl(r)}" style="color:var(--st-rec)">{ic("rec")}{b(r)}</span><span class="chip sq lrn" style="color:var(--st-dub)">{ic("dub")}</span><span class="chip sq{cl(p)}" style="color:var(--st-play)">{ic("play")}{b(p)}</span><span class="chip sq lrn">{ic("stop")}</span><span class="chip hear{" on" if n in ("voice","chor","keys") else ""}{cl(h)}">{ic("hear")}Mithören{b(h)}</span><div class="grow"></div><span class="knob s lrn" style="--v:0"></span></div>
</div></div>''')
    return '<div class="heads">' + "\n".join(out) + '<div class="addtrack"><span class="btn s">' + ic("plus") + 'Spur hinzufügen</span></div></div>'

lanes = gm.lanes().replace('<div class="donezone" style="--pos:18.33"></div>', '').replace('<div class="playhead" style="--pos:18.33"></div>', '<div class="playhead stopped" style="--pos:0"></div>')
tb = titlebar(learn=True, lrn_marks=True)
tb = tb.replace('title="Start">' + ic("start") + '</span>', 'title="Start">' + ic("start") + '<span class="mb mb-at inactive">FS 3</span></span>')
tb = tb.replace('title="Alles stoppen">' + ic("stopall") + '</span>', 'title="Alles stoppen">' + ic("stopall") + '<span class="mb mb-at inactive">FS 4</span></span>')
tb = tb.replace(ic("release") + 'Release</span>', ic("release") + 'Release<span class="mb mb-at inactive">FS 1</span></span>')
tb = tb.replace('title="Klick">' + ic("metro") + '</span>', 'title="Klick">' + ic("metro") + '<span class="mb mb-at">CC 21</span></span>')

side = f'''<aside class="side">
  <div class="side-h">{ic("midi","--ic:18px;color:var(--learn)")}<h3>MIDI-Belegung</h3><div class="grow"></div><span class="btn s">Profil: Bühne{ic("chev-d")}</span></div>
  <div class="dev missing">{ic("warn")}<div><b>Gerät fehlt: Fußleiste FS-4</b><span>3 Belegungen inaktiv · zuletzt gesehen 20:41</span></div><span></span></div>
  <div class="dev" style="margin-top:8px">{ic("pedal")}<div><b>Pad-Controller 16</b><span>verbunden · 7 Belegungen aktiv</span></div><span class="pill" style="height:20px"><i class="led"></i>aktiv</span></div>
  <div style="margin:10px 14px 0;display:flex;gap:6px"><span class="btn s">{ic("search")}Erneut suchen</span><span class="btn s">Auf anderes Gerät übertragen</span></div>
  <div class="hint" style="margin:10px 14px 0">Die Belegungen bleiben gespeichert und werden sofort wieder aktiv, sobald die Fußleiste angeschlossen ist. Bis dahin: <b>Release per Leertaste</b> oder Touch.</div>
  <div class="blist">
    <div class="bgrp caps">Transport</div>
    {brow("Release", "transport.next", "FS 1", off=True)}
    {brow("Start", "transport.start", "FS 3", off=True)}
    {brow("Alles stoppen", "transport.stop_all", "FS 4", off=True)}
    <div class="bgrp caps">Spuren</div>
    {brow("gitarre · Aufnahme", "track.1.record", "Pad 40")}
    {brow("gitarre · Wiedergabe", "track.1.play", "Pad 41")}
    {brow("voice · Overdub", "track.2.overdub", "Pad 37")}
    {brow("voice · Wiedergabe", "track.2.play", "Pad 38")}
    {brow("chor · Mithören", "track.3.monitor", "CC 20")}
    {brow("keys · Aufnahme", "track.5.record", "Pad 44")}
    <div class="bgrp caps">Global</div>
    {brow("Klick an/aus", "global.click", "CC 21")}
  </div>
  <div class="mon">
    <div class="caps" style="margin-bottom:4px">Eingangsmonitor</div>
    <div class="ln"><span>20:58:31</span><span>Pad-Contr.</span><b>Note 40 an · v 112</b></div>
    <div class="ln"><span>20:58:33</span><span>Pad-Contr.</span><b>CC 21 = 127</b></div>
  </div>
</aside>'''
learnbar = f'<div class="learnbar">{ic("target")}<b>MIDI-Learn aktiv.</b> Markiertes Element anklicken, dann Taste oder Regler betätigen. Clips sind keine MIDI-Ziele.<div class="grow"></div><span class="pill warn" style="margin-right:8px"><i class="led"></i>Fußleiste fehlt · 3 inaktiv</span><span class="btn s">Fertig</span></div>'

html = head("Henrys Looper · MIDI-Gerät fehlt") + f'''<body class="learn-on">
<!-- Mockup 5: MIDI-Mapping-Modus über dem Arrangement, Zustand „Gerät fehlt“. Belegungen der fehlenden
     Fußleiste sind durchgestrichen/gestrichelt (inaktiv), die des verbundenen Pad-Controllers aktiv. -->
<div class="window">
{tb}
  <div style="display:grid;grid-template-rows:auto 1fr;min-height:0">
  {learnbar}
  <div class="main learn" style="min-height:0">
    <div class="editor"><div class="arr" style="--rowh:84px">
      <div class="corner"><div class="row"><span class="caps">Spuren</span><span class="dim2" style="font-size:10px">5 / 8</span></div><div class="row"><span class="dim" style="font-size:11px">gestoppt</span></div></div>
      {ruler(pos=0, stopped=True)}
      {heads()}
      {lanes}
    </div></div>
    {side}
  </div>
  </div>
</div>
</body></html>
'''
open(D + "midi-missing.html", "w").write(html)
