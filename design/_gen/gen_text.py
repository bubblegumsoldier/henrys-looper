from common import *
import re
Y = r'''# Henrys 3/4-Song: Gitarre-Loop, dann drei Voice-Ebenen stapeln.
title: Henry 3/4
bpm: 141
time_signature: 3/4

tracks:
  gitarre: {input: 2}
  voice:   {input: 1}
  chor:    {input: 1, pan: 0.25}
  perc:    {input: 3}
  keys:    {input: [5, 6]}

sections:
  - id: gitarre_rec
    bars: 8
    autorelease: true
    tracks: {gitarre: record, voice: hear_through, perc: record}

  - id: voice_1
    bars: 8
    autorelease: true
    tracks: {gitarre: play, voice: record, perc: play}

  - id: voice_2
    bars: 8
    autorelease: true
    tracks: {gitarre: play, voice: overdub, perc: play, keys: hear_through}

  # Ebene 3. Loopt, bis du den Release-Knopf drückst.
  - id: voice_3
    repeat: voice_2
    autorelease: false
    quantize: loop
    tracks: {keys: recrod}

  - id: alles
    bars: 8
    autorelease: false
    quantize: bar'''
def hl(line):
    if line.strip().startswith("#"):
        return f'<span class="y-c">{line}</span>'
    s = line
    s = re.sub(r'\b(record)\b', r'<span class="y-rec">\1</span>', s)
    s = re.sub(r'\b(overdub)\b', r'<span class="y-dub">\1</span>', s)
    s = re.sub(r'\b(play)\b', r'<span class="y-play">\1</span>', s)
    s = re.sub(r'\b(hear_through)\b', r'<span class="y-hear">\1</span>', s)
    s = re.sub(r'\b(true|false)\b', r'<span class="y-b">\1</span>', s)
    s = re.sub(r'(?<![\w-])(\d+(?:\.\d+)?(?:/\d+)?)(?![\w])', r'<span class="y-n">\1</span>', s)
    s = re.sub(r'^(\s*(?:- )?)([a-z_]+):', r'\1<span class="y-k">\2</span>:', s)
    s = re.sub(r'([{,]\s*)([a-z_]+):', r'\1<span class="y-k">\2</span>:', s)
    s = s.replace("recrod", '<span class="squig y-s">recrod</span>')
    return s
lines = Y.split("\n")
ERR = next(i for i, l in enumerate(lines) if "recrod" in l)
gut = "".join(f'<div class="{"cur err" if i == ERR else ""}">{i+1}</div>' for i in range(len(lines)))
code = "".join(f'<span class="ln{" cur" if i == ERR else ""}">{hl(l) or " "}</span>' for i, l in enumerate(lines))
# Blockvorschau
cells = {"rec": "var(--st-rec)", "dub": "var(--st-dub)", "play": "var(--st-play)", "stop": "var(--bg-4)", "hear": "var(--st-hear)"}
pv = '<div class="pv">'
pv += '<div class="pv-r pv-h"><span></span>' + "".join(f'<span style="flex:{max(n,2)};text-align:center">{k+1}</span>' for k, (sid, s, n, a, q) in enumerate(SECS)) + "</div>"
for t in TRACKS:
    n = t[0]
    pv += f'<div class="pv-r"><span class="pv-t" style="--c:{t[1]}">{n}</span>'
    for j, (sid, s, nn, a, q) in enumerate(SECS):
        st = PLAN[n][j]
        err = (n == "keys" and j == 3)
        pv += f'<span class="pv-c{" err" if err else ""}" style="flex:{max(nn,2)};--k:{cells[st]}">{ic(st) if not err else ic("warn")}</span>'
    pv += "</div>"
pv += "</div>"
html = head("Henrys Looper · Partitur als Text") + f'''<body>
<!-- Mockup 7: Die YAML-Textansicht als gleichwertige zweite Sicht derselben Partitur. Umschalter
     „Raster | Text“ in der Werkzeugleiste; Fehler unter dem Cursor, Blockvorschau rechts live. -->
<div class="window">
{titlebar(dirty=True)}
  <div class="workspace">
    <div class="toolbar">
      <div class="seg"><span>{ic("grid")}Raster</span><span class="on">{ic("code")}Text (YAML)</span></div>
      <div class="sep"></div>
      <span class="dim" style="font-size:11px">henry-3-4.rust.yaml</span>
      <div class="grow"></div>
      <span class="lbl">Beide Ansichten bearbeiten dieselbe Partitur – Änderungen erscheinen sofort im Raster.</span>
      <div class="sep"></div>
      <span class="btn s">{ic("save")}Sichern</span>
    </div>
    <div class="main">
      <div class="code" style="position:relative">
        <div class="gut">{gut}</div>
        <pre>{code}</pre>
        <div class="errpop" style="left:290px;top:{10 + ERR*20 - 48}px"><b>Unbekannter Zustand „recrod“</b> für Spur <span class="mono">keys</span> in Sektion <span class="mono">voice_3</span>.<br>Erlaubt: <span class="mono">record</span>, <span class="mono">overdub</span>, <span class="mono">play</span>, <span class="mono">stop</span>, <span class="mono">hear_through</span>.<div style="margin-top:8px;display:flex;gap:6px"><span class="btn s accent">Ersetzen durch „record“</span></div></div>
      </div>
      <aside class="insp">
        <div class="insp-h"><div class="kicker"><span class="caps">Blockvorschau</span><span class="pill warn" style="height:20px"><i class="led"></i>1 Fehler</span></div>
          <div class="sub" style="margin-top:6px">Zeigt die Partitur, wie die Engine sie lesen würde. Die letzte gültige Fassung bleibt aktiv, bis der Fehler behoben ist.</div></div>
        <div class="insp-s">{pv}<div class="hint">Spalten = Sektionen 1–6 (Breite nach Takten), Zeilen = Spuren.</div></div>
        <div class="insp-s">
          <span class="caps">Meldungen</span>
          <div class="diag err">{ic("warn")}<div><b>Zeile {ERR+1}:</b> Unbekannter Zustand „recrod“.</div></div>
          <div class="diag">{ic("info")}<div><b>Zeile 30:</b> voice_3 erbt 8 Takte und alle Zustände von voice_2.</div></div>
          <div class="diag">{ic("info")}<div><b>Zeile 11:</b> keys ist stereo (Eingänge 5 und 6).</div></div>
        </div>
      </aside>
    </div>
    <div class="statusbar"><span>Zeile {ERR+1}, Spalte 24</span><span>YAML · UTF-8</span><div class="grow"></div><span style="color:var(--warn)">{ic("warn")} 1 Fehler · letzte gültige Fassung aktiv</span></div>
  </div>
</div>
</body></html>
'''
open(D + "arrangement-text.html", "w").write(html)
