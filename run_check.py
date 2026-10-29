"""Запуск проверочного скрипта на весах (обход кавычек PowerShell)."""
import io
import os
import sys

sys.stdout.reconfigure(encoding="utf-8", errors="replace")
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from scale_ssh import connect

script = sys.argv[1] if len(sys.argv) > 1 else "/root/wd-check.sh"
client = connect()
sftp = client.open_sftp()
data = open(sys.argv[1] if len(sys.argv) > 1 else "audit/src-bundle/wd-check.sh", "rb").read().replace(b"\r\n", b"\n")
# S11: не /tmp — файл под root читает только root (tmp общедоступен).
sftp.putfo(io.BytesIO(data), "/root/wd-check.sh")
sftp.close()
stdin, stdout, stderr = client.exec_command("sudo -S -p '' bash /root/wd-check.sh", timeout=300)
stdin.write(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".env")).read()
            .split("SCALE_PASSWORD=")[1].splitlines()[0] + "\n")
stdin.channel.shutdown_write()
print(stdout.read().decode())
err = stderr.read().decode().strip()
if err:
    print("ERR:", err)
client.close()
