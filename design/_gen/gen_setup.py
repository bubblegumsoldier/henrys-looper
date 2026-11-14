from common import *
def sel(t, w=None):
    st = f' style="width:{w}px"' if w else ""
    return f'<span class="field sel"{st}>{t}{ic("chev-d","")}</span>'.replace('<i class="ic ic-chev-d"></i>', '<i class="ic ic-chev-d chev"></i>')
rows = ""
lat = ["7,3", "7,3", "7,3", "7,3", "—"]
for i, (n, c, inp, ch, pre, let) in enumerate(TRACKS):
    hear = n in ("voice", "chor", "keys")
    inpsel = sel(inp.replace("In ", "Eingang "), 120)
    meas = f'<span class="num">{lat[i]} ms</span> <span class="dim" style="font-size:11px">gemessen</span>' if lat[i] != "—" else '<span class="btn s">Messen</span>'
    rows += f'''<tr><td><span class="tn2" style="--c:{c}"><i></i>{n}</span></td><td>{inpsel}</td><td><div class="seg" style="--ctl-h:22px"><span class="{"on" if ch=="mono" else ""}">mono</span><span class="{"on" if ch=="stereo" else ""}">stereo</span></div></td>
<td><span class="toggle{" on" if hear else ""}"></span></td><td>{sel("Main + Monitor", 140)}</td><td>{sel("Monitor", 110)}</td><td>{meas}</td><td><span class="field" style="width:70px">+0,0<span class="unit">ms</span></span></td></tr>'''
html = head("Henrys Looper · Einrichtung") + f'''<body>
<!-- Mockup 6: Einrichtung als eigene, ruhige Seite im selben Fenster. Links Navigation, rechts Karten. -->
<div class="window">
{titlebar(compact=True, status='<span class="pill"><i class="led"></i>Engine läuft · 48 kHz · 128</span>')}
<div class="setup">
  <nav class="snav">
    <div class="caps" style="padding:4px 10px 8px">Einrichtung</div>
    <a class="on" href="#">{ic("speaker")}Audio</a>
    <a href="#">{ic("input")}Spuren &amp; Eingänge<span class="badge">5</span></a>
    <a href="#">{ic("midi")}MIDI<span class="badge">2 Geräte</span></a>
    <a href="#">{ic("plug")}Plugins<span class="badge">AU · VST3</span></a>
    <a href="#">{ic("folder")}Sitzungen</a>
    <a href="#">{ic("gear")}Allgemein</a>
    <div class="grow"></div>
    <div class="hint" style="padding:0 10px">Änderungen an Gerät, Samplerate oder Puffer starten die Engine neu. Während eine Partitur läuft, sind sie gesperrt.</div>
  </nav>
  <main class="spage">
    <div><h1>Audio</h1><p class="lead">Interface, Taktung und wohin die beiden Busse gehen.</p></div>
    <section class="card">
      <div class="card-h"><h3>Audio-Gerät</h3><div class="grow"></div><span class="pill"><i class="led"></i>verbunden</span></div>
      <div class="card-b">
        <div class="srow"><label>Treiber</label><div>{sel("CoreAudio", 220)}</div></div>
        <div class="srow"><label>Gerät</label><div>{sel("Audio-Interface (8 In · 8 Out)", 320)}<span class="dim" style="font-size:12px">8 Eingänge · 8 Ausgänge</span></div></div>
        <div class="srow"><label>Samplerate</label><div><div class="seg"><span>44,1 kHz</span><span class="on">48 kHz</span><span>88,2 kHz</span><span>96 kHz</span></div></div></div>
        <div class="srow"><label>Puffer</label><div><div class="seg"><span>32</span><span>64</span><span class="on">128</span><span>256</span><span>512</span></div><span class="dim" style="font-size:12px">Samples</span></div></div>
        <div class="srow"><label>Latenz</label><div class="lat"><span>Eingang <b>3,4 ms</b></span><span>Ausgang <b>3,9 ms</b></span><span>Rundweg <b>7,3 ms</b></span><span>Aussetzer seit Start <b style="color:var(--ok)">0</b></span></div></div>
      </div>
    </section>
    <section class="card">
      <div class="card-h"><h3>Ausgangsbusse</h3><span class="dim" style="font-size:12px">Der Klick liegt immer nur auf Monitor.</span></div>
      <table class="tt">
        <tr><th style="width:220px">Bus</th><th>Ausgang</th><th>Breite</th><th>Lautstärke</th><th style="width:220px">Pegel</th></tr>
        <tr><td><span class="tn2">{ic("speaker","--ic:15px")}Main · Saal</span></td><td>{sel("Ausgang 1–2", 140)}</td><td><div class="seg" style="--ctl-h:22px"><span>mono</span><span class="on">stereo</span></div></td><td><span class="num">0,0 dB</span></td><td><span class="hm" style="width:200px"><i style="--l:72%;--p:80%"></i><i style="--l:70%;--p:78%"></i></span></td></tr>
        <tr><td><span class="tn2">{ic("phones","--ic:15px")}Monitor · Kopfhörer <span class="chip" style="margin-left:4px">{ic("metro")}Klick</span></span></td><td>{sel("Ausgang 3–4", 140)}</td><td><div class="seg" style="--ctl-h:22px"><span>mono</span><span class="on">stereo</span></div></td><td><span class="num">−4,0 dB</span></td><td><span class="hm" style="width:200px"><i style="--l:64%;--p:71%"></i><i style="--l:62%;--p:70%"></i></span></td></tr>
      </table>
    </section>
    <section class="card">
      <div class="card-h"><h3>Spuren und Eingänge</h3><span class="dim" style="font-size:12px">5 von 8 · gilt für diese Partitur</span><div class="grow"></div><span class="btn s">{ic("plus")}Spur</span></div>
      <table class="tt">
        <tr><th>Spur</th><th>Eingang</th><th>Kanäle</th><th>Mithören</th><th>Loop auf</th><th>Mithören auf</th><th>Latenz</th><th>Zuschlag</th></tr>
        {rows}
      </table>
    </section>
  </main>
</div>
</div>
</body></html>
'''
open(D + "setup.html", "w").write(html)
