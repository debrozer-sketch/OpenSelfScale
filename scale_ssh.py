"""SSH-подключение к весам по данным из .env.

Использование:
    python scale_ssh.py "uname -a"            # выполнить команду
    python scale_ssh.py --sudo "команда"      # выполнить через sudo -S,
                                              # пароль уходит в stdin
Логин/пароль читаются из .env рядом со скриптом и никогда не печатаются.
"""
import shlex
import sys
from pathlib import Path

import paramiko


def load_env(path: Path) -> dict:
    env = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, _, value = line.partition("=")
        env[key.strip()] = value.strip()
    return env


def connect() -> paramiko.SSHClient:
    """Подключение по данным .env; используется и CLI, и скриптами деплоя."""
    env_path = Path(__file__).with_name(".env")
    if not env_path.exists():
        raise RuntimeError(".env не найден рядом со скриптом")
    cfg = load_env(env_path)
    missing = [k for k in ("SCALE_HOST", "SCALE_USER", "SCALE_PASSWORD") if not cfg.get(k)]
    if missing:
        raise RuntimeError("Не заполнены поля в .env: " + ", ".join(missing))
    client = paramiko.SSHClient()
    client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    client.connect(
        hostname=cfg["SCALE_HOST"],
        port=int(cfg.get("SCALE_PORT", "22")),
        username=cfg["SCALE_USER"],
        password=cfg["SCALE_PASSWORD"],
        timeout=10,
        banner_timeout=10,
        auth_timeout=10,
    )
    return client


def main() -> int:
    argv = sys.argv[1:]
    sudo = False
    if argv and argv[0] == "--sudo":
        sudo = True
        argv = argv[1:]
    if not argv:
        print('Использование: python scale_ssh.py ["--sudo"] "команда"', file=sys.stderr)
        return 2

    client = connect()
    cfg = load_env(Path(__file__).with_name(".env"))
    try:
        if sudo:
            cmd = "sudo -S -p '' bash -c " + shlex.quote(argv[0])
        else:
            cmd = argv[0]
        stdin, stdout, stderr = client.exec_command(cmd, timeout=240)
        if sudo:
            stdin.write(cfg["SCALE_PASSWORD"] + "\n")
            stdin.flush()
            stdin.channel.shutdown_write()
        out = stdout.read().decode("utf-8", errors="replace")
        err = stderr.read().decode("utf-8", errors="replace")
        code = stdout.channel.recv_exit_status()
        if out:
            print(out, end="")
        if err:
            print(err, end="", file=sys.stderr)
        return code
    finally:
        client.close()


if __name__ == "__main__":
    sys.exit(main())
