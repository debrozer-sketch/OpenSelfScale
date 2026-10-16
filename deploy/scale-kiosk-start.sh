#!/bin/sh
# Киоск на физическом экране: Xorg на tty1, клиент — kiosk.
# Cookie генерируется при каждом старте; VNC-зеркало берёт этот же файл.
AUTH=/run/scale-kiosk/X.auth
COOKIE=$(mcookie)
: > "$AUTH"
xauth -f "$AUTH" add :0 . "$COOKIE" >/dev/null 2>&1
chmod 600 "$AUTH"
export XAUTHORITY="$AUTH"
export SLINT_BACKEND=winit-femtovg
exec xinit /usr/local/bin/kiosk -- /usr/bin/Xorg :0 vt1 -nolisten tcp
