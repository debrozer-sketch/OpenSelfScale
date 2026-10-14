"""SSH-подключение к весам по данным из .env.

Использование:
    python scale_ssh.py "uname -a"            # выполнить команду
    python scale_ssh.py --sudo "команда"      # выполнить через sudo -S,
                                              # пароль уходит в stdin
Логин/пароль читаются из .env рядом со скриптом и никогда не печатаются.

Jump-хост (если весы не видны напрямую): SCALE_JUMP в .env. Авторизация
на jump — ключом SCALE_JUMP_KEY (по умолчанию ~/.ssh/scalefw_bastion),
иначе паролем SCALE_JUMP_PASSWORD, иначе SCALE_PASSWORD. Переменные
окружения SCALE_* перекрывают значения из .env.
"""
import os
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
    for key, value in os.environ.items():
        if key.startswith("SCALE_") and value:
            env[key] = value
    return env


def _connect_jump(cfg: dict) -> paramiko.SSHClient:
    """Jump-хост: ключ предпочтительнее пароля."""
    jump = paramiko.SSHClient()
    jump.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    kwargs = dict(
        hostname=cfg["SCALE_JUMP"],
        port=int(cfg.get("SCALE_JUMP_PORT", "22")),
        username=cfg.get("SCALE_JUMP_USER", cfg["SCALE_USER"]),
        timeout=15,
        banner_timeout=15,
        auth_timeout=15,
        allow_agent=False,
        look_for_keys=False,
    )
    key_path = cfg.get("SCALE_JUMP_KEY") or str(
        Path.home() / ".ssh" / "scalefw_bastion"
    )
    if Path(key_path).exists():
        kwargs["pkey"] = paramiko.Ed25519Key.from_private_key_file(key_path)
    else:
        kwargs["password"] = cfg.get("SCALE_JUMP_PASSWORD", cfg["SCALE_PASSWORD"])
    jump.connect(**kwargs)
    return jump


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
    sock = None
    if cfg.get("SCALE_JUMP"):
        sock = _connect_jump(cfg).get_transport().open_channel(
            "direct-tcpip",
            (cfg["SCALE_HOST"], int(cfg.get("SCALE_PORT", "22"))),
            ("127.0.0.1", 0),
            timeout=15,
        )
    client.connect(
        hostname=cfg["SCALE_HOST"],
        port=int(cfg.get("SCALE_PORT", "22")),
        username=cfg["SCALE_USER"],
        password=cfg["SCALE_PASSWORD"],
        sock=sock,
        timeout=10,
        banner_timeout=10,
        auth_timeout=10,
        allow_agent=False,
        look_for_keys=False,
    )
    return client


def main() -> int:
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.reconfigure(encoding="utf-8", errors="replace")
        except (AttributeError, ValueError):
            pass
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
