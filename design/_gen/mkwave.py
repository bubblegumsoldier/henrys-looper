import random, math, sys
# Erzeugt zwei Wellenform-Masken (Gitarre: gleichmäßiges Anschlagen, Stimme: Phrasen) als SVG data-URI.
def wave(seed, n, shape):
    random.seed(seed)
    W, H = 400, 100
    pts_top, pts_bot = [], []
    for i in range(n + 1):
        x = i * W / n
        t = i / n
        a = shape(t) * (0.55 + 0.45 * random.random())
        a = max(0.03, min(0.96, a))
        pts_top.append((x, 50 - a * 48))
        pts_bot.append((x, 50 + a * 48 * (0.85 + 0.15 * random.random())))
    d = "M0 50 " + " ".join(f"L{x:.1f} {y:.1f}" for x, y in pts_top) + f" L{W} 50 " + " ".join(
        f"L{x:.1f} {y:.1f}" for x, y in reversed(pts_bot)) + " Z"
    svg = f"<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 400 100' preserveAspectRatio='none'><path d='{d}' fill='%23000'/></svg>"
    return svg.replace('<', '%3C').replace('>', '%3E')

def guitar(t):  # 24 Schläge (8 Takte 3/4), Anschlag mit Abklingen
    ph = (t * 24) % 1
    return 0.25 + 0.6 * math.exp(-ph * 3.5)
def voice(t):   # Phrasen mit Pausen
    seg = (t * 8) % 2
    if seg > 1.55: return 0.04
    return 0.35 + 0.45 * math.sin(math.pi * min(seg / 1.55, 1)) ** 0.6
def perc(t):
    ph = (t * 24) % 1
    beat = int(t * 24) % 3
    return (0.9 if beat == 0 else 0.45) * math.exp(-ph * 9) + 0.04
print("--wave-gtr:url(\"data:image/svg+xml," + wave(3, 240, guitar) + "\");")
print("--wave-vox:url(\"data:image/svg+xml," + wave(7, 200, voice) + "\");")
print("--wave-perc:url(\"data:image/svg+xml," + wave(11, 300, perc) + "\");")
