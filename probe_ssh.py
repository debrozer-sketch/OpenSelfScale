"""Read-only SSH-пробник для весов, отличной от основной машины.

Хост и пользователь передаются аргументами, пароль берётся из .env рядом
со скриптом (тот же, что и для 172.32.0.5). Команда выполняется «как есть»,
поэтому вызывать только читающие команды: ls, cat, stat, crontab -l, strings.

Использование:
    python probe_ssh.py "команда"
    python probe_ssh.py --sudo "команда"
"""
import shlex
import sys
from pathlib import Path

import paramiko

for stream in (sys.stdout, sys.stderr):
    try:
        stream.reconfigure(encoding="utf-8", errors="replace")
    except (AttributeError, ValueError):
        pass

ENV_KEYS = ("SCALE_HOST", "SCALE_USER", "SCALE_PASSWORD")


def load_env(path: Path) -> dict:
    env = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, _, value = line.partition("=")
        env[key.strip()] = value.strip()
    return env


def connect(host: str, user: str, port: int = 22) -> paramiko.SSHClient:
    cfg = load_env(Path(__file__).with_name(".env"))
    password = cfg.get("SCALE_PASSWORD")
    if not password:
        raise RuntimeError("SCALE_PASSWORD не заполнен в .env")
    client = paramiko.SSHClient()
    client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    client.connect(
        hostname=host,
        port=port,
        username=user,
        password=password,
        timeout=15,
        banner_timeout=15,
        auth_timeout=15,
    )
    return client


def run(host: str, user: str, command: str, sudo: bool = False, timeout: int = 120) -> int:
    client = connect(host, user)
    cfg = load_env(Path(__file__).with_name(".env"))
    try:
        cmd = "sudo -S -p '' bash -c " + shlex.quote(command) if sudo else command
        stdin, stdout, stderr = client.exec_command(cmd, timeout=timeout)
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


def main() -> int:
    argv = sys.argv[1:]
    sudo = False
    rest = []
    for arg in argv:
        if arg == "--sudo":
            sudo = True
        else:
            rest.append(arg)
    if len(rest) < 3:
        print(
            'Использование: python probe_ssh.py <хост> <пользователь> [--sudo] "команда"',
            file=sys.stderr,
        )
        return 2
    host, user, command = rest[0], rest[1], rest[2]
    return run(host, user, command, sudo=sudo)


if __name__ == "__main__":
    sys.exit(main())