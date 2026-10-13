# Удалённая установка Debian 13 на весы (без PXE, без флешки)

Работает, пока старая ОС грузится и есть SSH. Всё делается с этого ПК.

## Файлы

- `deploy/preseed.cfg` — сценарий установки (wipe /dev/sda, ESP+swap+root ext4,
  статический IP 172.32.0.5, hostname `scales`, openssh-server, root-пароль
  подставляется из .env при сборке).
- `deploy/make_initrd.py` — зашивает preseed в initrd установщика
  (второй gzip-cpio сегмент; ядро читает конкатенацию, d-i при
  `auto=true priority=critical` берёт /preseed.cfg). Результат:
  `deploy/netboot/initrd-preseed.gz`.
- `deploy/push.py` — SFTP-загрузка в `/home/administrator/install/` на весах.
- `deploy/netboot/vmlinuz`, `initrd.gz` — ядро/initrd d-i из netinst ISO
  (extract 7z: `install.amd/vmlinuz`, `install.amd/initrd.gz`).
- `deploy/pkgs/kexec-tools.deb` — kexec для Ubuntu 16.04 (xenial, old-releases).

## Процедура

```bash
# 1. Собрать initrd с preseed (пароль из .env, в репозиторий не попадает)
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
#    затем: ssh root@172.32.0.5 (тот же пароль, что и раньше).
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
