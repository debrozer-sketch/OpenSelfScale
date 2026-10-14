"""Пересборка индекса Packages(.xz) для оффлайн-зеркала deploy/xorg.

Аналог `dpkg-scanpackages . /dev/null` для машины без dpkg: читает control
из каждого *.deb (ar + tar.xz) и пишет Packages с полями Filename/Size/MD5/SHA*.
Запуск: python deploy/make_packages.py [каталог]  (по умолчанию deploy/xorg)
"""
import hashlib
import io
import lzma
import sys
import tarfile
from pathlib import Path

AR_MAGIC = b"!<arch>\n"


def deb_control(path: Path) -> str:
    data = path.read_bytes()
    if not data.startswith(AR_MAGIC):
        raise ValueError(f"не ar-архив: {path}")
    pos = 8
    while pos < len(data):
        hdr = data[pos : pos + 60]
        name = hdr[0:16].decode("ascii").strip()
        size = int(hdr[48:58].decode("ascii").strip())
        body = data[pos + 60 : pos + 60 + size]
        pos += 60 + size + (size % 2)
        if name in ("debian-binary", "data.tar.xz"):
            continue
        if name.startswith("control.tar"):
            with tarfile.open(fileobj=io.BytesIO(body)) as tar:
                for member in ("./control", "control"):
                    try:
                        return tar.extractfile(member).read().decode("utf-8")
                    except KeyError:
                        continue
            raise ValueError(f"control не найден: {path}")
    raise ValueError(f"control.tar.* не найден: {path}")


def main() -> int:
    root = Path(sys.argv[1] if len(sys.argv) > 1 else Path(__file__).parent / "xorg")
    debs = sorted(root.glob("*.deb"))
    if not debs:
        print(f"нет *.deb в {root}", file=sys.stderr)
        return 1
    entries = []
    for deb in debs:
        control = deb_control(deb)
        blob = deb.read_bytes()
        fields = {
            "Filename": f"{deb.name}",
            "Size": str(len(blob)),
            "MD5sum": hashlib.md5(blob).hexdigest(),
            "SHA1": hashlib.sha1(blob).hexdigest(),
            "SHA256": hashlib.sha256(blob).hexdigest(),
        }
        text = control.strip("\n") + "\n" + "\n".join(f"{k}: {v}" for k, v in fields.items())
        entries.append(text + "\n")
        print(f"  {deb.name}")
    packages = "\n".join(entries)
    (root / "Packages").write_text(packages, encoding="utf-8")
    (root / "Packages.xz").write_bytes(lzma.compress(packages.encode("utf-8")))
    print(f"OK: {len(debs)} пакетов -> Packages, Packages.xz")
    return 0


if __name__ == "__main__":
    sys.exit(main())
