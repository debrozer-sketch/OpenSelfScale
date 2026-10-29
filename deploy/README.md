# Удалённая установка Debian 13 на весы (без PXE, без флешки)

Работает, пока старая ОС грузится и есть SSH. Всё делается с этого ПК.

## Файлы

- `deploy/preseed.cfg` — сценарий установки (wipe /dev/sda, ESP+swap+root ext4,
  статический IP 192.0.2.10, hostname `scales`, openssh-server, root-пароль
  подставляется из .env при сборке). IP весов правится под объект перед
  сборкой initrd; в репозитории — документированный адрес 192.0.2.10.
- `deploy/make_initrd.py` — зашивает preseed в initrd установщика
  (второй gzip-cpio сегмент; ядро читает конкатенацию, d-i при
  `auto=true priority=critical` берёт /preseed.cfg). Результат:
  `deploy/netboot/initrd-preseed.gz`.
- `deploy/push.py` — SFTP-загрузка в `/home/administrator/install/` на весах.
- `deploy/netboot/vmlinuz`, `initrd.gz` — ядро/initrd d-i из netinst ISO
  (extract 7z: `install.amd/vmlinuz`, `install.amd/initrd.gz`).
- `deploy/pkgs/kexec-tools.deb` — kexec для Ubuntu 16.04 (xenial, old-releases).

## Процедура

В `.env` перед сборкой:

- `SCALE_SSH_PUBKEY` — публичный ключ деплой-машины (обязателен, ставится root'у).
- `SCALE_SSH_MODE` — `key` (по умолчанию; root SSH только по ключу,
  `PermitRootLogin prohibit-password`) или `password` (как раньше, `PermitRootLogin yes`).
- `SCALE_KEY` — путь к приватному ключу; если задан, `scale_ssh.py` ходит
  по ключу, иначе по паролю.

```bash
# 1. Собрать initrd с preseed (пароль/ключ из .env, в репозиторий не попадает)
python deploy/make_initrd.py

# 2. Загрузить на весы: ядро, initrd, ISO, kexec-tools
python deploy/push.py deploy/netboot/vmlinuz deploy/netboot/initrd-preseed.gz \
                      deploy/pkgs/kexec-tools.deb \
                      "E:/iventoy/iso/other/debian-13.7.0-amd64-netinst.iso"

# 3. kexec установлен? (dpkg -i если нет)
python scale_ssh.py --sudo "dpkg -i /home/administrator/install/kexec-tools.deb"

# 4. Прыжок в установщик (без ребута, UEFI не трогается)
python scale_ssh.py --sudo "kexec -l /home/administrator/install/vmlinuz \
  --initrd=/home/administrator/install/initrd-preseed.gz \
  --command-line='auto=true priority=critical console=tty0' && kexec -e"

# 5. SSH отвалится (установщик забрал консоль). Ждать 10-20 минут,
#    затем: ssh root@192.0.2.10 (тот же пароль, что и раньше).
#    Проверка: cat /etc/os-release, df -h, ls /home/administrator/sss (нет!)
```

## Что останется на диске после установки

Только новый Debian 13: ESP 512M, swap 1G, root ext4 (всё остальное место).
Старая система, её файлы, база, логи — уничтожены переразметкой.

## Если установка зависла / весы умерли

- kexec-прыжок происходит мгновенно: SSH умирает сразу, это норма.
- Установщик идёт по preseed без вопросов; мониторить нечем, кроме
  экрана весов и появления SSH на 22 порту.
- Если что-то пошло не так и весы не поднялись — физически перезагрузить
  питанием: загрузчик старой системы уничтожен, дальше только PXE-путь
  (уровень 2 из docs/RECOVERY.md) или флешка.

## Зеркало пакетов для установки (192.0.2.20:8000)

Установщик и apt весов берут пакеты с HTTP-зеркала на рабочем ПК
(каталог с deploy/xorg/*deb + Packages.xz; поднимается python -m http.server 8000).
Состав зеркала обязан включать всё из pkgsel/include (openssh-server, mc)
и зависимости: mc-data, libssl3t64, openssl-provider-legacy, libssh2-1t64,
libslang2, libgpm2, libext2fs2t64, gcc-14-base, libglib2.0-0t64, libatomic1.
Индекс пересобирать после каждого пополнения (dpkg-scanpackages на любой
Debian-машине или python-скрипт). Перед переустановкой весы проверять:
зеркало доступно, пакеты на месте.


## VNC-порт (5900)

`openscale-vnc.service` слушает 5900 без `-localhost`: экран киоска нужен
удалённо (торговый зал без монитора за стеклом). Порт не наружу —
только внутри доверенного сегмента 192.0.2.30.x.x за jump-хостом. Держать
порт открытым наружу или в сегменте с посторонними машинами нельзя;
в этом случае добавить `-localhost yes` в ExecStart.

## Дата и время (sudo-хелпер)

Киоск задаёт время через единственный root-хелпер. Установка на весах:

`
install -m 755 deploy/openscale-settime /usr/local/bin/openscale-settime
install -m 440 deploy/openscale-settime.sudoers /etc/sudoers.d/openscale-settime
`

Режимы: openscale-settime "ДД.ММ.ГГГГ ЧЧ:ММ" (ручная установка),
openscale-settime ntp-on|ntp-off (вкл/выкл systemd-timesyncd). Аргумент
валидуется скриптом (скелет + диапазоны + реальность даты через GNU date),
ввод извне в shell не попадает. NTP-сервер своей сети — [time] server
в /etc/openscale/config.toml.

## Удалённый доступ VNC (включение/выключение из меню)

Киоск переключает VNC через sudo-хелпер с фиксированными аргументами:

`
install -m 755 deploy/openscale-vnc-toggle /usr/local/sbin/openscale-vnc-toggle
install -m 440 deploy/openscale-vnc-toggle.sudoers /etc/sudoers.d/openscale-vnc-toggle
`

Пункт «Удалённый доступ (VNC)» в разделе «Доступ и безопасность». Событие пишется
в журнал действий.

## Восстановление PIN администратора (файл-ключ на USB)

1. На весах: `cat /etc/machine-id` (записать при установке).
2. `python deploy/make_reset_key.py <machine-id> > openscale-reset.key`
3. Положить `openscale-reset.key` в корень USB-флешки, вставить в весы.
4. «Доступ и безопасность» → «Сбросить PIN ключом с USB» → PIN станет 12345,
   сменить. Событие пишется в журнал действий.
