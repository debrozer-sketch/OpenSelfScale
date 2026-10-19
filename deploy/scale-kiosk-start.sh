#!/bin/sh
# Киоск на физическом экране: Xorg на tty1, клиент — kiosk.
# Cookie генерируется при каждом старте; VNC-зеркало берёт этот же файл.
# Файл с cookie создаётся сразу с правами 600 (без окна, когда он читаем всем).
umask 077
AUTH=/run/scale-kiosk/X.auth
COOKIE=$(mcookie)
: > "$AUTH"
xauth -f "$AUTH" add :0 . "$COOKIE" >/dev/null 2>&1
chmod 600 "$AUTH"
export XAUTHORITY="$AUTH"
export SLINT_BACKEND=winit-femtovg
# Сироты прошлой сессии: watchdog убивает только xinit, а kiosk/Xorg
# (PAM-сессия) переезжают в сессионный scope, держат дисплей и порты —
# новый киоск тогда умирает молча. Чистим перед стартом.
pkill -f '^/usr/local/bin/kiosk' 2>/dev/null
pkill -f 'Xorg :0' 2>/dev/null
sleep 1
exec xinit /usr/local/bin/kiosk -- /usr/bin/Xorg :0 vt1 -nolisten tcp
