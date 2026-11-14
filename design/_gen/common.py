import os
D = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..") + "/"

def head(title, extra_css=()):
    links = "".join(f'<link rel="stylesheet" href="{c}">\n' for c in ("tokens.css", "icons.css", "waves.css", "app.css", *extra_css))
    return f'''<!doctype html>
<html lang="de">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
{links}</head>
'''

def ic(n, style=""):
    s = f' style="{style}"' if style else ""
    return f'<i class="ic ic-{n}"{s}></i>'

def titlebar(mode="arr", running=False, sec="1 / 6", secname="gitarre_rec", bar="1", bars="8", beat=1,
             learn=False, armed=False, lrn_marks=False, dirty=False, status=None, compact=False, doc_sub=None):
    L = (lambda extra="": f' lrn{extra}') if lrn_marks else (lambda extra="": "")
    beats = "".join(f'<i class="{"one " if i == 0 else ""}{"on" if i == beat - 1 else ""}"></i>' for i in range(3))
    sub = doc_sub or ('<span class="dirty"><i class="dot"></i>ungespeichert</span>' if dirty else '<span><i class="dot"></i>gespeichert</span>')
    transport = ""
    if not compact:
        transport = f'''<div class="sep"></div>
    <div class="transport">
      <span class="tp start{" on" if running else ""}{L(" bound")}" title="Start">{ic("start")}</span>
      <span class="tp{L(" bound")}" title="Alles stoppen">{ic("stopall")}</span>
      <span class="tp release{" armed" if armed else ""}{L(" bound")}">{ic("release")}Release</span>
    </div>
    <div class="lcd">
      <div class="sec" style="width:162px"><label>Sektion {sec}</label><b>{secname}</b></div>
      <div><label>Takt</label><b class="num">{bar}<small>/ {bars}</small></b></div>
      <div><label>Schlag</label><b class="num">{beat}<span class="beats">{beats}</span></b></div>
      <div><label>Tempo</label><b class="num">141<small>BPM</small></b></div>
      <div><label>Taktart</label><b class="num">3/4</b></div>
    </div>
    <span class="btn sq on{L(" bound")}" title="Klick">{ic("metro")}</span>'''
    st = status or '<span class="pill"><i class="led"></i>48 kHz · 128 · 0 Aussetzer</span>'
    return f'''  <header class="titlebar">
    <div class="lights"><i></i><i></i><i></i></div>
    <div class="doc"><b>Henry 3/4</b>{sub}</div>
    {transport}
    <div class="grow"></div>
    <div class="seg mode"><span class="{"on" if mode == "arr" else ""}">{ic("grid")}Arrangement</span><span class="{"on" if mode == "live" else ""}">{ic("play-o")}Live</span></div>
    <div class="grow"></div>
    <span class="btn learn-btn{" on" if learn else ""}">{ic("midi")}MIDI-Learn</span>
    {st}
    <span class="btn sq ghost">{ic("gear")}</span>
  </header>
'''

TRACKS = [
    # name, color, input, ch, preset, letters
    ("gitarre", "var(--trk-1)", "In 2", "mono", "Gitarre", "HEK-R"),
    ("voice", "var(--trk-2)", "In 1", "mono", "Stimme", "HEKDR"),
    ("chor", "var(--trk-3)", "In 1", "mono", "Stimme", "HEKDR"),
    ("perc", "var(--trk-4)", "In 3", "mono", "Eigene", "-EK--"),
    ("keys", "var(--trk-5)", "In 5–6", "stereo", "Trocken", "-----"),
]
# Sektionen: id, start, takte, auto, quantize-label
SECS = [("gitarre_rec", 0, 8, True, None), ("voice_1", 8, 8, True, None), ("voice_2", 16, 8, True, None),
        ("voice_3", 24, 8, False, "Loop"), ("alles", 32, 8, False, "Takt"), ("ende", 40, 1, False, None)]
# Soll je Sektion × Spur
PLAN = {
    "gitarre": ["rec", "play", "play", "play", "play", "stop"],
    "voice": ["hear", "rec", "dub", "dub", "play", "stop"],
    "chor": ["stop", "stop", "stop", "stop", "hear", "stop"],
    "perc": ["rec", "play", "play", "play", "play", "stop"],
    "keys": ["stop", "stop", "hear", "rec", "play", "stop"],
}
LABEL = {"rec": "Aufnahme", "dub": "Overdub", "play": "Wiedergabe", "stop": "Still", "hear": "Mithören"}
ICON = {"rec": "rec", "dub": "dub", "play": "play", "stop": "stop", "hear": "hear"}

def fxl(letters):
    return '<span class="fxl">' + "".join(f'<span class="{"on" if c != "-" else ""}">{c if c != "-" else "–"}</span>' for c in letters) + "</span>"

def de(x):
    if abs(x) < 0.05: return "0,0"
    return ("+" if x > 0 else "−") + f"{abs(x):.1f}".replace(".", ",")
