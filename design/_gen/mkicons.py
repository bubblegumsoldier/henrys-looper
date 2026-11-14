import urllib.parse, sys
S = 'fill="none" stroke="#000" stroke-linecap="round" stroke-linejoin="round"'
F = 'fill="#000"'
icons = {
 # Zustände
 "rec": f'<circle cx="12" cy="12" r="7.5" {F}/>',
 "dub": f'<circle cx="12" cy="12" r="8" {S} stroke-width="2.4"/><circle cx="12" cy="12" r="3.6" {F}/>',
 "play": f'<path d="M7.5 4.8v14.4a.8.8 0 0 0 1.2.7l11.3-7.2a.8.8 0 0 0 0-1.4L8.7 4.1a.8.8 0 0 0-1.2.7z" {F}/>',
 "play-o": f'<path d="M8 5.5v13l10.5-6.5z" {S}/>',
 "stop": f'<rect x="5.5" y="5.5" width="13" height="13" rx="1.8" {F}/>',
 "hear": f'<rect x="9" y="2.5" width="6" height="11.5" rx="3" {F}/><path d="M5.5 11a6.5 6.5 0 0 0 13 0M12 17.5V21M8.5 21h7" {S}/>',
 "empty": f'<circle cx="12" cy="12" r="7.5" {S} stroke-dasharray="3 3"/>',
 "armed": f'<path d="M6.5 3h11M6.5 21h11M7.5 3c0 5.5 9 5.5 9 9s-9 3.5-9 9M16.5 3c0 5.5-9 5.5-9 9s9 3.5 9 9" {S}/>',
 # Transport
 "start": f'<path d="M7.5 4.8v14.4a.8.8 0 0 0 1.2.7l11.3-7.2a.8.8 0 0 0 0-1.4L8.7 4.1a.8.8 0 0 0-1.2.7z" {F}/>',
 "release": f'<path d="M4.5 5.5v13l9.5-6.5z" {F}/><rect x="16" y="5" width="3.2" height="14" rx="1" {F}/>',
 "stopall": f'<rect x="5" y="5" width="14" height="14" rx="2" {F}/>',
 "tostart": f'<rect x="4.8" y="5" width="3.2" height="14" rx="1" {F}/><path d="M19.5 5.5v13L10 12z" {F}/>',
 "metro": f'<path d="M8.6 3.5h6.8L19 20.5H5z" {S}/><path d="M12 15.5l6-8.5M8 16.5h8" {S}/>',
 "loop": f'<path d="M4 12a6 6 0 0 1 6-6h8M15 3l3 3-3 3M20 12a6 6 0 0 1-6 6H6M9 21l-3-3 3-3" {S}/>',
 "bar": f'<path d="M5 4v16M12 4v16M19 4v16" {S} stroke-width="1.6"/><path d="M5 12h14" {S} stroke-width="1.6" stroke-dasharray="2 2.5"/>',
 "auto": f'<path d="M4 12h12M12 7l5 5-5 5M20 5v14" {S}/>',
 "hand": f'<path d="M8 13V5.5a1.5 1.5 0 0 1 3 0V11M11 10V4a1.5 1.5 0 0 1 3 0v6M14 10V5.5a1.5 1.5 0 0 1 3 0V14a7 7 0 0 1-7 7h-.5a6 6 0 0 1-4.6-2.2L4 15.5a1.6 1.6 0 0 1 2.4-2l1.6 1.6" {S}/>',
 # Routing
 "speaker": f'<path d="M3.5 9.5h3.5L12 5v14l-5-4.5H3.5z" {F}/><path d="M15.5 9a4 4 0 0 1 0 6M18 6.5a7.5 7.5 0 0 1 0 11" {S}/>',
 "phones": f'<path d="M4 16v-3.5a8 8 0 0 1 16 0V16" {S}/><rect x="3" y="13.5" width="5" height="7" rx="1.6" {F}/><rect x="16" y="13.5" width="5" height="7" rx="1.6" {F}/>',
 "input": f'<path d="M3 12h11M10 8l4 4-4 4" {S}/><rect x="16" y="4" width="5" height="16" rx="1.5" {S}/>',
 "link": f'<path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1" {S}/>',
 # Allgemein
 "midi": f'<circle cx="12" cy="12" r="9" {S}/><circle cx="7.6" cy="12" r="1.3" {F}/><circle cx="16.4" cy="12" r="1.3" {F}/><circle cx="9" cy="8.2" r="1.3" {F}/><circle cx="15" cy="8.2" r="1.3" {F}/><circle cx="12" cy="7" r="1.3" {F}/><rect x="10.5" y="17" width="3" height="2" rx=".5" {F}/>',
 "gear": f'<circle cx="12" cy="12" r="3.2" {S}/><path d="M12 2.8v2.6M12 18.6v2.6M2.8 12h2.6M18.6 12h2.6M5.5 5.5l1.9 1.9M16.6 16.6l1.9 1.9M5.5 18.5l1.9-1.9M16.6 7.4l1.9-1.9" {S}/><circle cx="12" cy="12" r="6.5" {S}/>',
 "plus": f'<path d="M12 5v14M5 12h14" {S}/>',
 "minus": f'<path d="M5 12h14" {S}/>',
 "x": f'<path d="M6 6l12 12M18 6L6 18" {S}/>',
 "check": f'<path d="M5 12.5l4.5 4.5L19 7.5" {S} stroke-width="2.4"/>',
 "chev-d": f'<path d="M7 10l5 5 5-5" {S}/>',
 "chev-r": f'<path d="M10 7l5 5-5 5" {S}/>',
 "chev-l": f'<path d="M14 7l-5 5 5 5" {S}/>',
 "arr-l": f'<path d="M19 12H5M11 6l-6 6 6 6" {S}/>',
 "arr-r": f'<path d="M5 12h14M13 6l6 6-6 6" {S}/>',
 "dup": f'<rect x="8" y="8" width="12" height="12" rx="2" {S}/><path d="M16 8V5.5A1.5 1.5 0 0 0 14.5 4h-9A1.5 1.5 0 0 0 4 5.5v9A1.5 1.5 0 0 0 5.5 16H8" {S}/>',
 "trash": f'<path d="M4.5 6.5h15M9.5 6.5V4h5v2.5M6.5 6.5l1 13.5h9l1-13.5M10 10.5v6M14 10.5v6" {S}/>',
 "warn": f'<path d="M12 3.5l9.5 16.5h-19z" {S}/><path d="M12 10v4.5" {S}/><circle cx="12" cy="17.2" r="1.2" {F}/>',
 "plug": f'<path d="M9 3v5M15 3v5M6 8h12v2.5a6 6 0 0 1-12 0zM12 16.5V21" {S}/>',
 "sliders": f'<path d="M6 4v16M12 4v16M18 4v16" {S}/><rect x="3.5" y="13" width="5" height="3.2" rx="1" {F}/><rect x="9.5" y="7" width="5" height="3.2" rx="1" {F}/><rect x="15.5" y="11" width="5" height="3.2" rx="1" {F}/>',
 "grid": f'<rect x="3.5" y="5" width="17" height="14" rx="2" {S}/><path d="M3.5 10h17M3.5 14.5h17M10 5v14" {S} stroke-width="1.6"/>',
 "code": f'<path d="M8.5 7L3.5 12l5 5M15.5 7l5 5-5 5M13.5 4.5l-3 15" {S}/>',
 "lock": f'<rect x="5" y="10.5" width="14" height="10" rx="2" {F}/><path d="M8 10.5V8a4 4 0 0 1 8 0v2.5" {S}/>',
 "layers": f'<path d="M12 3.5l9 4.5-9 4.5-9-4.5z" {S}/><path d="M3 12l9 4.5 9-4.5M3 16l9 4.5 9-4.5" {S}/>',
 "target": f'<circle cx="12" cy="12" r="7.5" {S}/><circle cx="12" cy="12" r="2.2" {F}/><path d="M12 1.5v4M12 18.5v4M1.5 12h4M18.5 12h4" {S}/>',
 "folder": f'<path d="M3.5 7a1.5 1.5 0 0 1 1.5-1.5h4.5l2 2.5H19a1.5 1.5 0 0 1 1.5 1.5V17.5A1.5 1.5 0 0 1 19 19H5a1.5 1.5 0 0 1-1.5-1.5z" {S}/>',
 "save": f'<path d="M5 4h11l3.5 3.5V19a1 1 0 0 1-1 1h-13a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1z" {S}/><path d="M8 4v5h7V4M7.5 20v-6h9v6" {S}/>',
 "more": f'<circle cx="5.5" cy="12" r="1.7" {F}/><circle cx="12" cy="12" r="1.7" {F}/><circle cx="18.5" cy="12" r="1.7" {F}/>',
 "grip": f'<circle cx="9" cy="6" r="1.4" {F}/><circle cx="15" cy="6" r="1.4" {F}/><circle cx="9" cy="12" r="1.4" {F}/><circle cx="15" cy="12" r="1.4" {F}/><circle cx="9" cy="18" r="1.4" {F}/><circle cx="15" cy="18" r="1.4" {F}/>',
 "cpu": f'<rect x="6" y="6" width="12" height="12" rx="2" {S}/><path d="M9.5 3v3M14.5 3v3M9.5 18v3M14.5 18v3M3 9.5h3M3 14.5h3M18 9.5h3M18 14.5h3" {S}/>',
 "pedal": f'<rect x="3.5" y="8" width="17" height="11" rx="2.5" {S}/><circle cx="8.5" cy="13.5" r="2.2" {F}/><circle cx="15.5" cy="13.5" r="2.2" {F}/><path d="M12 8V4" {S}/>',
 "waves": f'<path d="M3 12h2M7 8v8M11 5v14M15 9v6M19 7v10M21 12h0" {S}/>',
 "search": f'<circle cx="10.5" cy="10.5" r="6" {S}/><path d="M15 15l5 5" {S}/>',
 "power": f'<path d="M12 3v8M7 6.3a7.5 7.5 0 1 0 10 0" {S}/>',
 "info": f'<circle cx="12" cy="12" r="9" {S}/><path d="M12 11v6" {S}/><circle cx="12" cy="7.8" r="1.2" {F}/>',
 "eye": f'<path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z" {S}/><circle cx="12" cy="12" r="3" {S}/>',
 "split": f'<rect x="3.5" y="4.5" width="17" height="15" rx="2" {S}/><path d="M12 4.5v15" {S}/>',
 "mute": f'<path d="M3.5 9.5h3.5L12 5v14l-5-4.5H3.5z" {F}/><path d="M16 9.5l5 5M21 9.5l-5 5" {S}/>',
 "keys": f'<rect x="3" y="5" width="18" height="14" rx="2" {S}/><path d="M8 5v14M13 5v14M18 5v8" {S} stroke-width="1.5"/>',
}
out = ["/* Icons — generiert aus design/_tools/mkicons.py nicht nötig: Datei ist die Quelle.",
       "   Gebrauch: <i class=\"ic ic-rec\"></i>. Farbe = currentColor, Größe = --ic (Standard 1em).",
       "   Jedes Icon ist ein Inline-SVG als Maske, keine externe Datei, keine Abhängigkeit. */",
       ".ic{--ic:1em;display:inline-block;flex:none;width:var(--ic);height:var(--ic);background:currentColor;"
       "-webkit-mask:var(--m) center/contain no-repeat;mask:var(--m) center/contain no-repeat;vertical-align:-0.14em}"]
for k, v in icons.items():
    svg = f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" stroke-width="2">{v}</svg>'
    enc = svg.replace('"', "'").replace('#', '%23').replace('<', '%3C').replace('>', '%3E')
    out.append(f'.ic-{k}{{--m:url("data:image/svg+xml,{enc}")}}')
open(sys.argv[1], "w").write("\n".join(out) + "\n")
print(len(icons))
