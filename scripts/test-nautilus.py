#!/usr/bin/env python3
"""Check Nautilus URI forwarding and the tar installer's launcher variant."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


repo = Path(__file__).resolve().parents[1]


with tempfile.TemporaryDirectory(prefix="localsend-nautilus-") as temporary:
    root = Path(temporary)
    home = root / "home"
    data = root / "data"
    binary_dir = root / "bin"
    binary_dir.mkdir()
    capture = root / "argv.json"
    binary = binary_dir / "localsend-gtk"
    binary.write_text(
        "#!/usr/bin/python3\n"
        "import json, os, sys\n"
        "from pathlib import Path\n"
        'Path(os.environ["LOCALSEND_ARGV_CAPTURE"]).write_text(json.dumps(sys.argv[1:]))\n',
        encoding="utf-8",
    )
    binary.chmod(0o755)
    # Avoid talking to the caller's notification service during rejection cases.
    notify = binary_dir / "notify-send"
    notify.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    notify.chmod(0o755)
    env = os.environ.copy()
    env.pop("NAUTILUS_SCRIPT_SELECTED_URIS", None)
    env.update(
        HOME=str(home), XDG_DATA_HOME=str(data),
        XDG_CONFIG_HOME=str(root / "config"),
        XDG_CACHE_HOME=str(root / "cache"),
        LOCALSEND_ARGV_CAPTURE=str(capture),
        PATH=str(binary_dir) + ":/usr/bin:/bin",
    )
    archive = root / "archive"
    files = {
        "install.sh": "packaging/install.sh",
        "share/applications/org.localsend.localsend_gtk.desktop":
            "packaging/rpm/localsend-gtk.desktop",
        "share/kio/servicemenus/localsend-dolphin.desktop":
            "packaging/desktop/localsend-dolphin.desktop",
        "share/nautilus-scripts/Send with LocalSend":
            "packaging/desktop/nautilus-scripts/Send with LocalSend",
        "share/icons/hicolor/512x512/apps/localsend-gtk.png": "assets/logo.png",
        "share/doc/localsend-gtk/README.md": "README.md",
    }
    for destination, source in files.items():
        target = archive / destination
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(repo / source, target)
    (archive / "bin").mkdir()
    shutil.copy2(binary, archive / "bin/localsend-gtk")
    prefix = home / ".local" / "prefix ' \" $ ` % \\"
    subprocess.run(["bash", str(archive / "install.sh"), str(prefix)],
                   env=env, capture_output=True, text=True, check=True)

    source_script = archive / "share/nautilus-scripts/Send with LocalSend"
    installed_script = data / "nautilus/scripts/Send with LocalSend"
    paths = [root / name for name in (
        "--help", "space name.txt", "中文.txt", "line\nbreak.txt",
        "ends-with-newline\n", "$`'\"\\%f%u%25?.txt", "folder with spaces",
    )]
    uris = "\n".join(path.as_uri() for path in paths) + "\n"
    rejected = ("", "\n", "smb://server/share/file", "file://other-host/file",
                "file:///bad%", "file:///bad%0", "file:///bad%GG",
                "file:///bad%00name", "file:///file?query", "file:///file#fragment",
                paths[0].as_uri() + "\nsmb://server/share/file\n")
    checks = 0
    for script in (source_script, installed_script):
        subprocess.run(["bash", "-n", str(script)], check=True)

        def invoke(args, selection=None):
            capture.unlink(missing_ok=True)
            invocation_env = env.copy()
            if script == installed_script:
                # A custom-prefix installation must not depend on PATH lookup.
                invocation_env["PATH"] = "/usr/bin:/bin"
                # Retain only our harmless notification stub on PATH.
                quiet_dir = root / "quiet-bin"
                quiet_dir.mkdir(exist_ok=True)
                shutil.copy2(notify, quiet_dir / "notify-send")
                invocation_env["PATH"] = str(quiet_dir) + ":/usr/bin:/bin"
            if selection is not None:
                invocation_env["NAUTILUS_SCRIPT_SELECTED_URIS"] = selection
            return subprocess.run(["bash", str(script), *args], env=invocation_env,
                                  capture_output=True, text=True, timeout=5)

        for selection in (uris, uris.rstrip("\n"), uris.replace("file:///", "file://localhost/")):
            # Real Nautilus 46.4 changes %f in argv before the script starts.
            result = invoke(["misleading-file-after-field-code-expansion"], selection)
            assert result.returncode == 0, result.stderr
            assert json.loads(capture.read_text()) == ["--", *map(str, paths)]
            checks += 1
        for selection in rejected:
            result = invoke(["must-not-be-sent"], selection)
            assert result.returncode != 0, selection
            assert not capture.exists(), selection
            assert result.stderr.startswith("LocalSend: "), result.stderr
            checks += 1
        manual = ["--help", "space name.txt", "line\nbreak.txt", "%f"]
        result = invoke(manual)
        assert result.returncode == 0, result.stderr
        assert json.loads(capture.read_text()) == ["--", *manual]
        checks += 1

print(f"Nautilus source and installed launcher: {checks} forwarding/rejection checks passed")
