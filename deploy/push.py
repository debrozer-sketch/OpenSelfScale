"""Загрузка файлов на весы по SFTP (paramiko, креды из .env).
Использование:
    python deploy/push.py <локальный файл>[:<удалённый путь>] ...
    python deploy/push.py <бинарник> --deploy
Простой режим: файл кладётся в <домашний каталог SSH-пользователя>/install
(SFTP работает без root; установка в систему — потом через scale_ssh.py --sudo).

Режим --deploy: безопасное обновление киоска на /usr/local/bin/openscale:
  1. загрузка во временный файл install/openscale.new;
  2. сверка размера и sha256 на стороне весов;
  3. проверка запускаемости нового бинарника (openscale.new --version);
  4. сохранение текущего бинарника как openscale.prev;
  5. атомарная замена (mv внутри одной файловой системы /usr/local/bin);
  6. systemctl restart + проверка is-active;
  7. при неудачном запуске — автоматический возврат openscale.prev и
     повторный restart (данные киоска не затрагиваются).
"""
import hashlib
import shlex
import sys
from pathlib import Path
for _stream in (sys.stdout, sys.stderr):
    try:
        _stream.reconfigure(encoding="utf-8", errors="replace")
    except (AttributeError, ValueError):
        pass
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import scale_ssh

BIN = "/usr/local/bin/openscale"
SERVICE = "openscale-kiosk"
STAGING = "/usr/local/lib/openscale.staging"


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def sudo(client, cmd: str, timeout: int = 240) -> tuple[int, str, str]:
    cfg = scale_ssh.load_env(Path(scale_ssh.__file__).with_name(".env"))
    wrapped = "sudo -S -p '' bash -c " + shlex.quote(cmd)
    stdin, stdout, stderr = client.exec_command(wrapped, timeout=timeout)
    stdin.write(cfg["SCALE_PASSWORD"] + "\n")
    stdin.flush()
    code = stdout.channel.recv_exit_status()
    return code, stdout.read().decode(errors="replace"), stderr.read().decode(errors="replace")


def deploy(client: scale_ssh.paramiko.SSHClient, sftp, src: Path) -> int:
    want = sha256(src)
    size = src.stat().st_size
    dst = sftp.normalize(".") + "/install/openscale.new"
    print(f"→ загрузка {src.name}: {size} байт ...", flush=True)
    sftp.put(str(src), dst)
    got = sftp.stat(dst).st_size
    if got != size:
        print(f"  загрузка оборвана: {got} из {size} — обновление отменено", flush=True)
        return 1
    code, out, _ = sudo(client, f"sha256sum {shlex.quote(dst)}")
    remote = out.split()[0] if out.strip() else ""
    if code != 0 or remote != want:
        print(f"  контрольная сумма не совпала: {remote} ≠ {want} — обновление отменено", flush=True)
        return 1
    print(f"  sha256 OK: {want[:16]}…", flush=True)

    code, out, err = sudo(client, f"chmod 0755 {shlex.quote(dst)} && {shlex.quote(dst)} --version")
    if code != 0:
        print(f"  новый бинарник не запускается: {err.strip()} — обновление отменено", flush=True)
        return 1
    print(f"  проверка запуска: {out.strip()}", flush=True)

    code, _, err = sudo(
        client,
        f"mkdir -p {STAGING} && cp -f {shlex.quote(dst)} {STAGING}/openscale && "
        f"chmod 0755 {STAGING}/openscale && "
        f"if [ -f {BIN} ]; then cp -f {BIN} {BIN}.prev; fi && "
        f"mv -f {STAGING}/openscale {BIN}",
    )
    if code != 0:
        print(f"  замена не удалась: {err.strip()}", flush=True)
        return 1
    print(f"  заменён {BIN} (предыдущая версия: {BIN}.prev)", flush=True)

    code, _, _ = sudo(client, f"systemctl restart {SERVICE} && sleep 6 && systemctl is-active {SERVICE}")
    active = code == 0
    if not active:
        print("  сервис не поднялся — откат к предыдущему бинарнику", flush=True)
        sudo(client, f"mv -f {BIN}.prev {BIN} && systemctl restart {SERVICE}")
        code2, out2, _ = sudo(client, f"sleep 5 && systemctl is-active {SERVICE}")
        if code2 == 0:
            print("  откат выполнен, сервис работает на предыдущей версии", flush=True)
        else:
            print("  !!! откат тоже не поднял сервис — требуется ручное вмешательство", flush=True)
        return 1
    print(f"  сервис {SERVICE}: active, обновление завершено", flush=True)
    return 0


def main(argv: list[str]) -> int:
    deploy_mode = "--deploy" in argv
    argv = [a for a in argv if a != "--deploy"]
    if not argv:
        print("нет файлов для загрузки", file=sys.stderr)
        return 2
    client = scale_ssh.connect()
    sftp = client.open_sftp()
    remote_dir = sftp.normalize(".") + "/install"
    try:
        try:
            sftp.stat(remote_dir)
        except FileNotFoundError:
            sftp.mkdir(remote_dir)
        if deploy_mode:
            return deploy(client, sftp, Path(argv[0]))
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
