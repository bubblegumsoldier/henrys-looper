from gen_live import *

def brow(t, small, badge, cur=False, off=False):
    b = f'<span class="mb{" inactive" if off else ""}">{badge}</span>' if badge else '<span class="dim2" style="font-size:11px">wartet …</span>'
    return f'<div class="brow{" cur" if cur else ""}{" off" if off else ""}"><span class="t">{t}<small>{small}</small></span>{b}{ic("x")}</div>'

side = f'''<aside class="side">
  <div class="side-h">{ic("midi","--ic:18px;color:var(--learn)")}<h3>MIDI-Belegung</h3><div class="grow"></div><span class="btn s">Profil: Bühne{ic("chev-d")}</span></div>
  <div class="dev">{ic("pedal")}<div><b>2 Geräte verbunden</b><span>Fußleiste FS-4 · Pad-Controller 16</span></div><span class="pill" style="height:20px"><i class="led"></i>aktiv</span></div>
  <div class="waitcard">
    <span class="caps">Ziel gewählt</span>
    <div class="tgt">chor · Aufnahme</div>
    <div class="addr">track.3.record</div>
    <div class="wait"><span class="spinner"></span><div><b>Wartet auf Controller …</b><br><span class="dim" style="font-size:11px">Drück eine Taste, ein Pad oder bewege einen Regler.</span></div></div>
    <div class="acts"><span class="btn s">{ic("x")}Abbrechen</span><span class="btn s ghost">Belegung entfernen</span><div class="grow"></div><span class="dim" style="font-size:11px;align-self:center">Esc</span></div>
  </div>
  <div class="blist">
    <div class="bgrp caps">Transport</div>
    {brow("Release", "transport.next", "FS 1")}
    {brow("Start", "transport.start", "FS 3")}
    {brow("Alles stoppen", "transport.stop_all", "FS 4")}
    <div class="bgrp caps">Spuren</div>
    {brow("gitarre · Aufnahme", "track.1.record", "Pad 40")}
    {brow("gitarre · Wiedergabe", "track.1.play", "Pad 41")}
    {brow("voice · Overdub", "track.2.overdub", "Pad 37")}
    {brow("voice · Wiedergabe", "track.2.play", "Pad 38")}
    {brow("chor · Aufnahme", "track.3.record", None, cur=True)}
    {brow("chor · Mithören", "track.3.monitor", "CC 20")}
    {brow("keys · Aufnahme", "track.5.record", "Pad 44")}
    <div class="bgrp caps">Global</div>
    {brow("Kopfhörer-Lautstärke", "global.bus.monitor.gain", "CC 7")}
  </div>
  <div class="mon">
    <div class="caps" style="margin-bottom:4px;display:flex;justify-content:space-between"><span>Eingangsmonitor</span><span class="dim2" style="letter-spacing:0;text-transform:none;font-weight:500">Kanal 1–16</span></div>
    <div class="ln"><span>21:14:07</span><span>Fußleiste</span><b>Note 60 an · FS 1</b></div>
    <div class="ln"><span>21:14:09</span><span>Pad-Contr.</span><b>Note 37 an · v 96</b></div>
    <div class="ln"><span>21:14:12</span><span>Pad-Contr.</span><b>CC 7 = 88</b></div>
  </div>
</aside>'''
learnbar = f'<div class="learnbar">{ic("target")}<b>MIDI-Learn aktiv.</b> Markiertes Element anklicken, dann Taste oder Regler am Controller betätigen. Belegte Elemente tragen ein Abzeichen.<div class="grow"></div><span class="dim" style="color:#e3b6ef">Wiedergabe läuft normal weiter</span><span class="btn s" style="margin-left:8px">Fertig</span></div>'
open(D + "midi-learn.html", "w").write(page("Henrys Looper · MIDI-Learn", "Mockup 4: MIDI-Mapping-Modus über dem Live-Modus. Ziel „chor · Aufnahme“ ist gewählt, die App wartet auf den Controller. Belegte Elemente tragen Abzeichen, alle belegbaren sind gestrichelt markiert.", learn=True, target="chor.rec", side=side, learnbar=learnbar))
