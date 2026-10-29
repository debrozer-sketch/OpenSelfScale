#!/usr/bin/env python3
"""Генератор файла-ключа восстановления PIN администратора (B12).

На весах (по SSH или с консоли): cat /etc/machine-id
На своей машине: python deploy/make_reset_key.py <machine-id> > openscale-reset.key
Файл положить в корень USB-флешки. На весах: «Доступ и безопасность» →
«Сбросить PIN ключом с USB» → PIN администратора станет 12345.
"""
import hashlib
import sys


def main() -> None:
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        sys.exit(2)
    machine_id = sys.argv[1].strip().lower()
    digest = hashlib.sha256(b"openscale-reset:" + machine_id.encode()).hexdigest()
    print(digest)


if __name__ == "__main__":
    main()
