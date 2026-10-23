#!/bin/sh
xrandr --output eDP-1 --mode 1024x768 --pos 0x0 --panning 1024x768+0+0 2>/dev/null
i=0
while [ "$i" -lt 50 ]; do
    xrandr --fb 1536x768 >/dev/null 2>&1
    xrandr --fb 2048x768 >/dev/null 2>&1
    if xrandr 2>/dev/null | grep -aq 'current 2048 x 768'; then
        break
    fi
    i=$((i + 1))
    sleep 0.5
done
if [ "$i" -ge 50 ]; then
    echo "scale-kiosk-client: fb 2048x768 не поднялся, работаем на 1024x768" >&2
fi
xinput list --name-only | grep -a "CoolTouch" | while IFS= read -r D; do
    case "$D" in
        *Mouse*) xinput disable "$D" ;;
        *) xinput set-prop "$D" "Coordinate Transformation Matrix" 0.5 0 0 0 1 0 0 0 1 ;;
    esac
done
fails=0
while true; do
    t0=$(date +%s)
    /usr/local/bin/kiosk
    dt=$(( $(date +%s) - t0 ))
    if [ "$dt" -lt 2 ]; then
        fails=$((fails + 1))
        if [ "$fails" -ge 5 ]; then
            echo "scale-kiosk-client: kiosk падает при старте, перезапускаю X" >&2
            exit 1
        fi
    else
        fails=0
    fi
    sleep 1
done
