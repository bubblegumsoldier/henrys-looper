#!/bin/sh
# Rendert alle Mockups mit Headless-Chrome nach design/screenshots (Gerätepixel 2x).
# Aufruf: sh design/shoot.sh [name ...]   (ohne Argumente: alle)
cd "$(dirname "$0")"
CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
shot() { # datei breite hoehe ziel
  "$CHROME" --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor=2 \
    --window-size="$2,$3" --virtual-time-budget=1500 --screenshot="$PWD/screenshots/$4.png" \
    "file://$PWD/$1.html" >/dev/null 2>&1
  echo "screenshots/$4.png"
}
want() { [ $# -eq 0 ] && return 0; for a in $ARGS; do [ "$a" = "$1" ] && return 0; done; return 1; }
ARGS="$*"
for p in arrangement arrangement-mixer arrangement-text live midi-learn midi-missing setup; do
  [ -f "$p.html" ] || continue
  if [ -z "$ARGS" ] || want "$p"; then shot "$p" 1440 900 "$p-1440x900"; fi
done
if [ -f live.html ] && { [ -z "$ARGS" ] || want live; }; then shot live 1024 768 live-1024x768; fi
