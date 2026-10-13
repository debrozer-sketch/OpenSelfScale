"""Загрузка файлов на весы по SFTP (paramiko, креды из .env).

Использование:
    python deploy/push.py <локальный файл>[:<удалённый путь>] ...

Удалённый путь по умолчанию: /root/install/<имя файла>.
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import scale_ssh  # noqa: E402  (переиспользует .env и подключение)


def main(argv: list[str]) -> int:
    if not argv:
        print("нет файлов для загрузки", file=sys.stderr)
        return 2
    client = scale_ssh.connect()
    sftp = client.open_sftp()
    # Пишем в домашний каталог пользователя (SFTP работает без root),
    # root-операции потом через scale_ssh.py --sudo.
    remote_dir = sftp.normalize(".") + "/install"
    try:
        try:
            sftp.stat(remote_dir)
        except FileNotFoundError:
            sftp.mkdir(remote_dir)
        for spec in argv:
            if ":" in spec and Path(spec.split(":", 1)[0]).exists():
                local, remote = spec.split(":", 1)
            else:
                local, remote = spec, remote_dir + "/" + Path(spec).name
            src, dst = Path(local), remote
            size = src.stat().st_size
            print(f"→ {src.name}: {size} байт ...", flush=True)
            sftp.put(str(src), dst)
            got = sftp.stat(dst).st_size
            status = "OK" if got == size else f"РАЗМЕР НЕ СОВПАЛ ({got})"
            print(f"  {dst} — {status}", flush=True)
            if got != size:
                return 1
    finally:
        sftp.close()
        client.close()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
