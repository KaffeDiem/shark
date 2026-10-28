#!/usr/bin/env python3
"""Release gate for exact managed artifacts. Failed protocol checks block publishing."""
import hashlib
import io
import json
import os
import pathlib
import posixpath
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
CACHE = ROOT / "target" / "language-tool-check"


def relative(name):
    normalized = posixpath.normpath(name)
    if name.startswith("/") or normalized == ".." or normalized.startswith("../"):
        raise ValueError("archive path escapes installation: " + name)
    return normalized


def extract(archive, directory):
    # Explicit extraction works on macOS's older Python without tarfile's filter= API.
    links = []
    with tarfile.open(archive) as tar:
        members = tar.getmembers()
        if sum(member.size for member in members) > 1024 * 1024 * 1024:
            raise ValueError("archive is too large")
        for member in members:
            name = relative(member.name)
            target = directory / name
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            elif member.isfile():
                target.parent.mkdir(parents=True, exist_ok=True)
                with tar.extractfile(member) as source, target.open("xb") as output:
                    shutil.copyfileobj(source, output)
                target.chmod(member.mode & 0o777)
            elif member.issym() or member.islnk():
                base = posixpath.dirname(name) if member.issym() else ""
                resolved = relative(posixpath.join(base, member.linkname))
                links.append((target, member.linkname, directory / resolved, member.issym()))
            else:
                raise ValueError("unsupported archive member: " + member.name)
        # No archive file is ever written through a symlink.
        for target, link, resolved, symbolic in links:
            if not resolved.resolve().is_relative_to(directory.resolve()):
                raise ValueError("archive link escapes installation")
            target.parent.mkdir(parents=True, exist_ok=True)
            if symbolic:
                target.symlink_to(link)
            else:
                os.link(resolved, target)


def install(artifact):
    destination = CACHE / artifact["name"] / artifact["version"]
    executable = destination / artifact["executable"]
    marker = destination / ".verified"
    if executable.is_file() and marker.is_file() and marker.read_text() == artifact["sha256"]:
        return executable
    if destination.exists():
        raise ValueError("Move incomplete cache aside: " + str(destination))
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".stage-", dir=destination.parent) as temp:
        stage = pathlib.Path(temp)
        archive = stage / "download.tar.gz"
        digest = hashlib.sha256()
        total = 0
        deadline = time.monotonic() + 180
        if not artifact["url"].startswith("https://"):
            raise ValueError("artifact URL must use HTTPS")
        with urllib.request.urlopen(artifact["url"], timeout=30) as response, archive.open("xb") as output:
            if not response.url.startswith("https://"):
                raise ValueError("artifact redirected outside HTTPS")
            while True:
                chunk = response.read(1024 * 1024)
                if not chunk:
                    break
                total += len(chunk)
                if total > 256 * 1024 * 1024 or time.monotonic() > deadline:
                    raise ValueError("artifact download exceeded its limit")
                digest.update(chunk)
                output.write(chunk)
        if digest.hexdigest() != artifact["sha256"]:
            raise ValueError("checksum mismatch: " + artifact["name"])
        extract(archive, stage)
        archive.unlink()
        if not (stage / artifact["executable"]).is_file():
            raise ValueError("artifact executable is missing")
        (stage / ".verified").write_text(artifact["sha256"])
        stage.rename(destination)
    return executable


def check():
    manifest = json.loads((ROOT / "src/lsp/tool-manifest.json").read_text())
    programs = {artifact["name"]: install(artifact) for artifact in manifest}
    # A fixed classic SDK exists only in this test cache, never in a user's project.
    classic = install({"name": "typescript-classic-fixture", "version": "6.0.3", "url": "https://registry.npmjs.org/typescript/-/typescript-6.0.3.tgz", "sha256": "33cd0ee1beaa8c9e9d15a9da836c62ddea4c34a42d7c2d349dbc80d94165d22a", "executable": "package/lib/tsserver.js"})
    node = str(programs["node"])
    env = dict(os.environ, PATH=str(programs["node"].parent) + os.pathsep + os.environ.get("PATH", ""))
    cases = [
        ("Pyrefly", "python", [str(programs["pyrefly"]), "lsp"], {}),
        ("Ruff", "ruff", [str(programs["ruff"]), "server"], {}),
        ("Pyright", "python", [node, str(programs["pyright"]), "--stdio"], {}),
        ("Classic TypeScript", "typescript", [node, str(programs["typescript-language-server"]), "--stdio"], {"SHARK_LSP_SMOKE_INIT": json.dumps({"tsserver": {"path": str(classic)}})}),
        ("Native TypeScript", "typescript", [str(programs["typescript"]), "--lsp", "--stdio"], {}),
        ("gopls", "go", [str(ROOT / "target/language-tools/gopls/0.24.0/gopls")], {"GOTOOLCHAIN": "path"}),
    ]
    failures = []
    for name, language, command, extra in cases:
        result = subprocess.run([sys.executable, str(ROOT / "scripts/lsp-smoke.py"), language, "--"] + command, env=dict(env, **extra), timeout=180)
        if result.returncode:
            failures.append(name)
    # Toolchain-owned servers use the same selection as the app, without installing a compiler.
    for name, language, lookup in [("Rust", "rust", ["rustup", "which", "rust-analyzer"]), ("Swift", "swift", ["/usr/bin/xcrun", "--find", "sourcekit-lsp"])]:
        try:
            tool_env = dict(env, RUSTUP_AUTO_INSTALL="0")
            if language == "rust":
                active = subprocess.check_output(["rustup", "show", "active-toolchain"], cwd=ROOT, env=tool_env, timeout=15, text=True).split()[0]
                tool_env["RUSTUP_TOOLCHAIN"] = active
            program = subprocess.check_output(lookup, cwd=ROOT, env=tool_env, timeout=15, text=True).strip()
            result = subprocess.run([sys.executable, str(ROOT / "scripts/lsp-smoke.py"), language, "--", program], env=tool_env, timeout=180)
            if result.returncode:
                failures.append(name)
        except (subprocess.SubprocessError, OSError) as error:
            print(name + ": " + str(error), file=sys.stderr)
            failures.append(name)
    result = subprocess.run([sys.executable, str(ROOT / "scripts/bsp-smoke.py"), str(programs["sweetpad"])], env=env, timeout=180)
    if result.returncode:
        failures.append("SweetPad BSP")
    if failures:
        raise SystemExit("Release blocked by language-tool gates: " + ", ".join(failures))


def self_test():
    for name in ["/etc/passwd", "../escape", "nested/../../escape"]:
        try:
            relative(name)
        except ValueError:
            pass
        else:
            raise AssertionError("accepted unsafe path: " + name)
    with tempfile.TemporaryDirectory(prefix="shark-extract-proof-") as temp:
        root = pathlib.Path(temp)
        archive = root / "valid.tar"
        with tarfile.open(archive, "w") as tar:
            member = tarfile.TarInfo("lib/npm.js")
            member.size = 4
            tar.addfile(member, io.BytesIO(b"test"))
            member = tarfile.TarInfo("bin/npm")
            member.type = tarfile.SYMTYPE
            member.linkname = "../lib/npm.js"
            tar.addfile(member)
        target = root / "valid"
        target.mkdir()
        extract(archive, target)
        assert (target / "bin/npm").read_text() == "test"
        archive = root / "invalid.tar"
        with tarfile.open(archive, "w") as tar:
            member = tarfile.TarInfo("escape")
            member.type = tarfile.SYMTYPE
            member.linkname = "../outside"
            tar.addfile(member)
        try:
            extract(archive, target)
        except ValueError:
            pass
        else:
            raise AssertionError("accepted escaping symlink")
        assert not (root / "outside").exists()
    print("artifact extractor path/symlink checks passed")


if __name__ == "__main__":
    if sys.argv[1:] == ["--self-test"]:
        self_test()
    else:
        check()
