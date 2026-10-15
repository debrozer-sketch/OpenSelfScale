#!/bin/sh
# Клиент xinit. Схема "панель + VNC": фреймбуфер 2048x768, физическая
# панель (1024x768) прикована к левой половине (panning), тач сжат в неё.
# Окно настроек паркуется правее панели (1024,0) и видно только в VNC;
# на весах меню открывается переносом окна на (0,0).
# fb расширяется в два шага: прямой прыжок Xorg может отклонить молча.
# Гонка со стартом X: xrandr сразу после xinit может провалиться — ретраи
# с проверкой фактического размера скрина; без 2048 VNC видит только
# панель, киоск остаётся рабочим (меню переносится на панель).
# ВАЖНО: НЕ приколачивать panning к 1024x768 — вызов --panning со
# вьюпром меньше fb СХЛОПЫВАЕТ fb до вьюпорта. Нужное поведение даёт
# сам --fb: panning = 2048x768+0+0, панель показывает левый верхний угол.
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
# Перезапуск только приложения при живом X: меню настроек умеет
# перезагружать ПО (std::process::exit → Restart=always юнита). Если
# завершать клиент (exec kiosk), xinit умирает и systemd пересоздаёт
# весь юнит — экран моргает чёрным на каждый перезапуск из меню.
# Предохранитель: 5 падений короче 2 с подряд — отдаём юнит systemd,
# чтобы он пересоздал X (иначе бессмысленный цикл на мёртвом X).
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
