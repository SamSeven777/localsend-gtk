#!/usr/bin/env python3
"""Exercise real GApplication file startup on a private Wayland/D-Bus session."""
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time


binary = Path("target/debug/localsend-gtk").resolve()


def check_startup(directory, corrupt_settings=False):
    env = os.environ.copy()
    for kind in ("CONFIG", "DATA", "CACHE"):
        env[f"XDG_{kind}_HOME"] = str(directory / kind.lower())
    settings = directory / "config/localsend-gtk/settings.json"
    settings.parent.mkdir(parents=True)
    if corrupt_settings:
        settings.write_text("{invalid settings", encoding="utf-8")
    offered_file = directory / "file with spaces.txt"
    offered_file.write_text("file-open regression", encoding="utf-8")
    with socket.socket() as reserved_port, tempfile.TemporaryFile() as log:
        # Keep the receiver offline so this desktop test never discovers or
        # announces itself to real peers. Discovery starts only after a bind.
        reserved_port.bind(("0.0.0.0", 0))
        reserved_port.listen()
        if not corrupt_settings:
            settings.write_text(json.dumps({"port": reserved_port.getsockname()[1]}),
                                encoding="utf-8")
        proc = subprocess.Popen([str(binary), str(offered_file)], env=env,
                                stdout=log, stderr=log)
        try:
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline:
                if proc.poll() is not None:
                    raise AssertionError(f"Cold file-open exited: {proc.returncode}")
                owner = subprocess.run([
                    "gdbus", "call", "--session", "--dest", "org.freedesktop.DBus",
                    "--object-path", "/org/freedesktop/DBus", "--method",
                    "org.freedesktop.DBus.NameHasOwner", "org.localsend.localsend_gtk",
                ], env=env, capture_output=True, text=True, check=True, timeout=5)
                if "true" in owner.stdout:
                    break
                time.sleep(0.05)
            else:
                raise AssertionError("Application did not register on the private bus")
            # A second process must forward its file to the existing instance.
            if not corrupt_settings:
                subprocess.run([str(binary), offered_file.as_uri()], env=env,
                               stdout=log, stderr=log, check=True, timeout=15)
            # GTK signal-handler panics abort the primary process. Give the
            # asynchronous dispatch a chance to finish before checking it.
            time.sleep(1)
            assert proc.poll() is None, "File-open crashed the primary application"
            if corrupt_settings:
                assert settings.read_text(encoding="utf-8") == "{invalid settings"
        except BaseException:
            log.seek(0)
            print(log.read().decode(errors="replace"))
            raise
        finally:
            if proc.poll() is None:
                proc.terminate()
            proc.wait(timeout=10)


with tempfile.TemporaryDirectory(prefix="localsend-file-open-") as temporary:
    root = Path(temporary)
    check_startup(root / "normal")
    check_startup(root / "corrupt", corrupt_settings=True)
print("Cold file-open, forwarded URI and corrupt-settings error window passed")
