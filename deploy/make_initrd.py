"""Сборка установочного initrd с зашитым preseed для Debian d-i.
Принцип: к оригинальному initrd.gz ISO конкатенируется второй gzip-cpio
архив, содержащий /preseed.cfg. Ядро Linux разворачивает конкатенированные
cpio-архивы последовательно, d-i при auto=true priority=critical читает
/preseed.cfg автоматически. Паролей в initrd нет: root-вход только по
SSH-ключу (SCALE_SSH_PUBKEY из .env), адрес зеркала — MIRROR_IP.
Запуск из каталога проекта:  python deploy/make_initrd.py
Результат:                   deploy/netboot/initrd-preseed.gz
"""
import gzip
import sys
from pathlib import Path
ROOT = Path(__file__).resolve().parent.parent
SEED_SRC = ROOT / "deploy" / "preseed.cfg"
INITRD_SRC = ROOT / "deploy" / "netboot" / "debian-installer" / "amd64" / "initrd.gz"
INITRD_OUT = ROOT / "deploy" / "netboot" / "initrd-preseed.gz"
ENV_FILE = ROOT / ".env"
def load_env() -> dict[str, str]:
    env = {}
    for line in ENV_FILE.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        k, v = line.split("=", 1)
        env[k.strip()] = v.strip()
    return env
def cpio_newc_entry(name: str, data: bytes) -> bytes:
    """Одна запись newc (SVR4): magic + 13 полей по 8 hex-символов + имя + данные."""
    ino, mode = 1, 0o100644
    fields = [ino, mode, 0, 0, 1, 0, len(data), 0, 0, 0, 0, len(name) + 1, 0]
    header = b"070701" + "".join(f"{v:08X}" for v in fields).encode()
    assert len(header) == 110, len(header)
    out = header + name.encode() + b"\0"
    out += b"\0" * ((4 - len(out) % 4) % 4)
    out += data
    out += b"\0" * ((4 - len(data) % 4) % 4)
    return out
def cpio_trailer() -> bytes:
    return cpio_newc_entry("TRAILER!!!", b"")
def main() -> int:
    env = load_env()
    mirror = env.get("MIRROR_IP")
    if not mirror:
        print("нет MIRROR_IP в .env — не могу подставить адрес зеркала", file=sys.stderr)
        return 1
    pubkey = env.get("SCALE_SSH_PUBKEY")
    if not pubkey:
        print("нет SCALE_SSH_PUBKEY в .env — не могу поставить ключ root", file=sys.stderr)
        return 1
    # Пароль root в initrd не зашивается: вход только по ключу
    # (PermitRootLogin prohibit-password в preseed).
    seed = (
        SEED_SRC.read_text(encoding="utf-8")
        .replace("__MIRROR_IP__", mirror)
        .replace("__ROOT_PUBKEY__", pubkey)
    )
    for placeholder in ("__MIRROR_IP__", "__ROOT_PUBKEY__", "__ROOT_PW__", "__SSH_PERMIT__"):
        if placeholder in seed:
            print(f"плейсхолдер не заменён: {placeholder}", file=sys.stderr)
            return 1
    if "ROOT_PW" in seed or "PermitRootLogin yes" in seed:
        print("preseed содержит пароль root или password-вход — так нельзя", file=sys.stderr)
        return 1
    base = INITRD_SRC.read_bytes()
    seed_cpio = cpio_newc_entry("preseed.cfg", seed.encode("utf-8")) + cpio_trailer()
    out = base + gzip.compress(seed_cpio, mtime=0)
    INITRD_OUT.write_bytes(out)
    print(f"OK: {INITRD_OUT} ({len(out)} байт; base {len(base)}, preseed {len(seed)} байт)")
    return 0
if __name__ == "__main__":
    sys.exit(main())
