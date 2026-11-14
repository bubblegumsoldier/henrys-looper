from common import *

def ruler(sel=None, cur=None, pos=0.0, stopped=True, armed_at=None):
    out = []
    for i, (sid, s, n, auto, q) in enumerate(SECS):
        cls = "sec"
        if not auto: cls += " loopsec"
        if sel == i: cls += " sel"
        if cur == i: cls += " cur"
        meta = f'<span>{n} T</span>'
        if auto:
            meta += f'<span>{ic("auto")}auto</span>'
        else:
            meta += f'<span class="rel">{ic("hand")}{"Release" if n > 1 else ""}</span>'
            if q == "Loop": meta += f'<span>{ic("loop")}Loop</span>'
            if q == "Takt": meta += f'<span>{ic("bar")}Takt</span>'
        link = ic("link", "--ic:11px;color:var(--accent)") if sid == "voice_3" else ""
        pad = ';padding-left:5px' if n == 1 else ''
        out.append(f'<div class="{cls}" style="--s:{s};--n:{n}{pad}"><div class="t"><span class="no">{i+1}</span>{sid}{link}</div><div class="m">{meta}</div></div>')
    nums = []
    for b in range(0, 40, 2):
        cls = ' class="s"' if b % 8 == 0 else ""
        nums.append(f'<span{cls} style="left:calc(var(--x0) + {b} * var(--b))">{b+1}</span>')
    ph = f'<div class="playhead ruler-ph{" stopped" if stopped else ""}" style="--pos:{pos}"></div>'
    arm = f'<div class="armline" style="--pos:{armed_at}"></div>' if armed_at is not None else ""
    return f'''<div class="ruler">
{chr(10).join(out)}
<div class="bars">{"".join(nums)}</div>
<div class="ticks" style="right:auto;width:calc(41 * var(--b) + 1px)"></div>
{arm}{ph}
</div>'''

def clip(state, s, n, extra="", hd_right="", body="", cls=""):
    sz = " tiny" if n == 1 else ""
    label = "" if n == 1 else f"<span>{LABEL[state]}</span>"
    bd = "" if state == "stop" else f'<div class="bd">{body}</div>'
    return f'<div class="clip {state}{sz}{cls}" style="--s:{s};--n:{n}{extra}"><div class="hd">{ic(ICON[state])}{label}{hd_right}</div>{bd}</div>'
