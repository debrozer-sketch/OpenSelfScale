#!/bin/sh
umask 077
AUTH=/run/openscale-kiosk/X.auth
COOKIE=$(mcookie)
: > "$AUTH"
xauth -f "$AUTH" add :0 . "$COOKIE" >/dev/null 2>&1
chmod 600 "$AUTH"
export XAUTHORITY="$AUTH"
export SLINT_BACKEND=winit-femtovg
pkill -f '^/usr/local/bin/openscale' 2>/dev/null
pkill -f 'Xorg :0' 2>/dev/null
sleep 1
exec xinit /usr/local/bin/openscale -- /usr/bin/Xorg :0 vt1 -nolisten tcp
