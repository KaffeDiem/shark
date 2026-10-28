"""Local QA benchmark: process-cold Python LSP readiness, warm reads, tree RSS.
No install/download; uses exact artifacts already verified by check-language-tools.py.
"""

import contextlib
import json
import math
import os
import pathlib
import queue
import signal
import subprocess
import tempfile
import threading
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
MANIFEST = json.loads((ROOT / "src/lsp/tool-manifest.json").read_text())


def artifact(name):
    a = next(a for a in MANIFEST if a["name"] == name)
    cache = ROOT / "target/language-tool-check" / name / a["version"]
    assert (cache / ".verified").read_text() == a["sha256"], cache
    path = cache / a["executable"]
    assert path.is_file(), path
    return str(path)


class Peer:
    def __init__(self, command, root):
        self.stderr = tempfile.TemporaryFile()
        self.process = subprocess.Popen(
            command,
            cwd=root,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self.stderr,
            start_new_session=True,
        )
        self.messages = queue.Queue()
        self.ident = 0
        self.progress = {}

        def read():
            try:
                while True:
                    headers = {}
                    while True:
                        line = self.process.stdout.readline()
                        if not line:
                            raise EOFError("server exited")
                        if line in (b"\r\n", b"\n"):
                            break
                        key, value = line.decode().split(":", 1)
                        headers[key.lower()] = value.strip()
                    self.messages.put(
                        json.loads(
                            self.process.stdout.read(int(headers["content-length"]))
                        )
                    )
            except Exception as error:
                self.messages.put(error)

        threading.Thread(target=read, daemon=True).start()

    def write(self, value):
        data = json.dumps(dict(jsonrpc="2.0", **value)).encode()
        self.process.stdin.write(
            ("Content-Length: %d\r\n\r\n" % len(data)).encode() + data
        )
        self.process.stdin.flush()

    def notify(self, method, params):
        self.write(dict(method=method, params=params))

    def request(self, method, params=None):
        self.ident += 1
        ident = self.ident
        value = dict(method=method, id=ident)
        if params is not None:
            value["params"] = params
        self.write(value)
        deadline = time.monotonic() + 90
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(f"{method} exceeded its response deadline")
            value = self.messages.get(timeout=remaining)
            if isinstance(value, Exception):
                raise value
            if "method" in value and "id" in value:
                result = (
                    [None] * len(value.get("params", {}).get("items", []))
                    if value["method"] == "workspace/configuration"
                    else None
                )
                self.write(dict(id=value["id"], result=result))
            elif value.get("method") == "$/progress":
                p = value["params"]
                token = str(p["token"])
                kind = p["value"]["kind"]
                if kind == "end":
                    self.progress.pop(token, None)
                else:
                    self.progress[token] = p["value"]
            elif value.get("id") == ident:
                if "error" in value:
                    raise RuntimeError(value["error"])
                return value.get("result")

    def close(self):
        try:
            if self.process.poll() is None:
                try:
                    self.request("shutdown")
                    self.write(dict(method="exit"))
                    self.process.wait(timeout=10)
                finally:
                    if self.process.poll() is None:
                        with contextlib.suppress(ProcessLookupError):
                            os.killpg(self.process.pid, signal.SIGKILL)
                    self.process.wait()
            assert self.process.returncode == 0, self.process.returncode
        finally:
            self.stderr.close()


def rss_tree(roots):
    rows = [
        list(map(int, line.split()))
        for line in subprocess.check_output(
            ["/bin/ps", "-axo", "pid=,ppid=,rss="], text=True, timeout=5
        ).splitlines()
    ]
    selected = set(roots)
    while True:
        children = {pid for pid, parent, _ in rows if parent in selected}
        if children <= selected:
            break
        selected |= children
    return sum(rss for pid, _, rss in rows if pid in selected)


def stats(values):
    values = sorted(values)
    assert values
    return dict(
        p50_ms=values[(len(values) - 1) // 2],
        p95_ms=values[math.ceil(len(values) * 0.95) - 1],
        samples=len(values),
    )


def run(round_no):
    with tempfile.TemporaryDirectory(prefix="shark-lsp-perf-") as directory:
        root = pathlib.Path(directory)
        for i in range(500):
            (root / ("mod_%04d.py" % i)).write_text(
                "def answer() -> int:\n    return 42\n"
                + "".join(
                    "def item_%04d(value: int) -> int:\n    return value + %d\n"
                    % (j, j)
                    for j in range(100)
                )
            )
        text = (
            "".join("import mod_%04d\n" % i for i in range(500))
            + "value = mod_0000.answer()\n"
        )
        source = root / "main.py"
        source.write_text(text)
        roots, samples, phase, stop = [], [], ["cold-load"], threading.Event()

        sample_errors = []

        def sample():
            try:
                while not stop.is_set():
                    samples.append((phase[0], rss_tree(roots)))
                    stop.wait(0.05)
            except Exception as error:
                sample_errors.append(error)

        sampler = threading.Thread(target=sample, daemon=True)
        sampler.start()
        peers = []
        start = time.perf_counter()
        try:
            for command in [[artifact("pyrefly"), "lsp"], [artifact("ruff"), "server"]]:
                peer = Peer(command, root)
                peers.append(peer)
                roots.append(peer.process.pid)
                peer.request(
                    "initialize",
                    dict(
                        processId=os.getpid(),
                        rootUri=root.as_uri(),
                        workspaceFolders=[dict(uri=root.as_uri(), name="performance")],
                        capabilities=dict(
                            general=dict(positionEncodings=["utf-16"]),
                            window=dict(workDoneProgress=True),
                            workspace=dict(configuration=True),
                            textDocument=dict(
                                completion=dict(
                                    completionItem=dict(snippetSupport=False)
                                ),
                                publishDiagnostics=dict(versionSupport=True),
                            ),
                        ),
                        initializationOptions=None,
                    ),
                )
                peer.notify("initialized", {})
                peer.notify(
                    "textDocument/didOpen",
                    dict(
                        textDocument=dict(
                            uri=source.as_uri(),
                            languageId="python",
                            version=1,
                            text=text,
                        )
                    ),
                )
            primary = peers[0]
            params = dict(
                textDocument=dict(uri=source.as_uri()),
                position=dict(line=500, character=19),
            )
            definition = primary.request("textDocument/definition", params)
            assert definition, "cold definition missing"
            cold_ms = (time.perf_counter() - start) * 1000
            assert "mod_0000.py" in json.dumps(definition), definition
            phase[0] = "quiet-after-first-definition"
            time.sleep(2)
            idle_kib = rss_tree(roots)
            latencies = {}
            phase[0] = "warm-requests"
            for method in ["textDocument/completion", "textDocument/definition"]:
                values = []
                for _ in range(30):
                    before = time.perf_counter()
                    result = primary.request(method, params)
                    values.append((time.perf_counter() - before) * 1000)
                    if method == "textDocument/completion":
                        items = (
                            result.get("items", [])
                            if isinstance(result, dict)
                            else result
                        )
                        assert items and any(
                            item["label"].startswith("answer") for item in items
                        ), result
                    else:
                        assert "mod_0000.py" in json.dumps(result), result
                latencies[method] = stats(values)
            stop.set()
            sampler.join(timeout=5)
            assert not sampler.is_alive()
            assert not sample_errors, sample_errors
            print(
                json.dumps(
                    dict(
                        round=round_no,
                        fixture_files=501,
                        fixture_bytes=sum(p.stat().st_size for p in root.glob("*.py")),
                        cold_first_cross_file_definition_ms=cold_ms,
                        warm=latencies,
                        server_tree_idle_rss_mib=idle_kib / 1024,
                        server_tree_cold_sampled_peak_rss_mib=max(
                            v for p, v in samples if p == "cold-load"
                        )
                        / 1024,
                        server_tree_all_sampled_peak_rss_mib=max(v for _, v in samples)
                        / 1024,
                        memory_samples=len(samples),
                        progress_active_after_requests=bool(primary.progress),
                    )
                ),
                flush=True,
            )
            assert source.read_text() == text
            for peer in peers:
                peer.notify(
                    "textDocument/didClose",
                    dict(textDocument=dict(uri=source.as_uri())),
                )
        finally:
            stop.set()
            sampler.join(timeout=5)
            with contextlib.ExitStack() as cleanup:
                for peer in peers:
                    cleanup.callback(peer.close)


if __name__ == "__main__":
    for round_no in range(1, 4):
        run(round_no)
