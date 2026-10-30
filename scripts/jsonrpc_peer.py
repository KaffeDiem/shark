"""Content-Length framed JSON-RPC over a child's stdio, shared by the LSP/BSP scripts."""
import contextlib
import json
import os
import queue
import signal
import subprocess
import tempfile
import threading
import time


class Peer:
    def __init__(self, command, root):
        self.stderr = tempfile.TemporaryFile()
        # A separate process group lets failures reap child servers as well.
        self.process = subprocess.Popen(
            command,
            cwd=root,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self.stderr,
            start_new_session=True,
        )
        self.messages = queue.Queue()
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        try:
            while True:
                headers = {}
                while True:
                    line = self.process.stdout.readline()
                    if not line:
                        raise EOFError("server closed stdout")
                    if line in (b"\r\n", b"\n"):
                        break
                    key, value = line.decode().split(":", 1)
                    headers[key.lower()] = value.strip()
                self.messages.put(
                    json.loads(self.process.stdout.read(int(headers["content-length"])))
                )
        except Exception as error:
            self.messages.put(error)

    def write(self, value):
        raw = json.dumps(dict(jsonrpc="2.0", **value)).encode()
        self.process.stdin.write(("Content-Length: %d\r\n\r\n" % len(raw)).encode() + raw)
        self.process.stdin.flush()

    def send(self, method, params=None, ident=None):
        value = {"method": method}
        if params is not None:
            value["params"] = params
        if ident is not None:
            value["id"] = ident
        self.write(value)

    def receive(self, deadline):
        """The next message; raises the reader's error, or queue.Empty at the deadline."""
        value = self.messages.get(timeout=max(0.01, deadline - time.monotonic()))
        if isinstance(value, Exception):
            raise value
        return value

    def answer(self, request):
        """Null results; workspace/configuration gets one per requested item."""
        items = request.get("params", {}).get("items", [])
        result = [None] * len(items) if request["method"] == "workspace/configuration" else None
        self.write(dict(id=request["id"], result=result))

    def stderr_tail(self):
        self.stderr.seek(0)
        return self.stderr.read().decode(errors="replace")[-8000:]

    def kill(self):
        # Signal the group even if the leader already exited: its children may not have.
        # macOS reports EPERM, not ESRCH, when only the unreaped leader is left.
        with contextlib.suppress(ProcessLookupError, PermissionError):
            os.killpg(self.process.pid, signal.SIGKILL)
        self.process.wait()
