#!/usr/bin/env python3
"""Exercise an actual stdio server, with bounded waits and unsaved document changes.

Usage: python3 scripts/lsp-smoke.py typescript -- /path/to/tsc --lsp --stdio
"""
import json
import os
import pathlib
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from jsonrpc_peer import Peer  # noqa: E402


class ContentModified(Exception):
    """A server may cancel reads while its background project model changes."""


def exercise(language, command):
    fixtures = {
        "typescript": ("main.ts", "const answer: number = 1;\nconst value = answer;\n", "const answer: number = 2;\nconst value = answer;\n", 1, 16, {"tsconfig.json": '{"compilerOptions":{"strict":true}}'}),
        "python": ("main.py", "answer = 1\nvalue = answer\n", "answer = 2\nvalue = answer\n", 1, 9, {}),
        "go": ("main.go", "package main\nfunc answer() int { return 1 }\nfunc main() { _ = answer() }\n", "package main\nfunc answer() int { return 2 }\nfunc main() { _ = answer() }\n", 2, 20, {"go.mod": "module example.com/shark-lsp-proof\n\ngo 1.27.0\n"}),
        "rust": ("src/main.rs", "fn answer() -> i32 { 1 }\nfn main() { let _ = answer(); }\n", "fn answer() -> i32 { 2 }\nfn main() { let _ = answer(); }\n", 1, 22, {"Cargo.toml": '[package]\nname="shark-lsp-proof"\nversion="0.1.0"\nedition="2021"\n'}),
        "swift": ("Sources/App/main.swift", "let answer: Int = 1\nlet value = answer\n", "let answer: Int = 2\nlet value = answer\n", 1, 14, {"Package.swift": '// swift-tools-version: 6.0\nimport PackageDescription\nlet package = Package(name: "App", targets: [.executableTarget(name: "App")])\n'}),
        "ruff": ("main.py", "import os\nx=  1\n", "import sys\nx=  2\n", 0, 7, {}),
    }
    name, initial, edited, line, character, configs = fixtures[language]
    with tempfile.TemporaryDirectory(prefix="shark-lsp-proof-") as temp:
        root = pathlib.Path(temp)
        source = root / name
        source.parent.mkdir(parents=True, exist_ok=True)
        source.write_text(initial)
        for path, text in configs.items():
            (root / path).write_text(text)
        uri = source.as_uri()
        peer = Peer(command, root)
        process, send = peer.process, peer.send
        lifecycle_deadline = time.monotonic() + 120
        diagnostic_messages = []

        def response(ident):
            deadline = min(time.monotonic() + 60, lifecycle_deadline)
            while True:
                value = peer.receive(deadline)
                if "method" in value and "id" in value:
                    peer.answer(value)
                elif value.get("method") == "textDocument/publishDiagnostics":
                    diagnostic_messages.append(value)
                elif value.get("id") == ident:
                    if value.get("error", {}).get("code") == -32801:
                        raise ContentModified()
                    assert "error" not in value, value
                    return value.get("result")
                if time.monotonic() >= deadline:
                    raise TimeoutError("server response timed out")

        try:
            send("initialize", {"processId": os.getpid(), "rootUri": root.as_uri(), "workspaceFolders": [{"uri": root.as_uri(), "name": root.name}], "initializationOptions": json.loads(os.environ.get("SHARK_LSP_SMOKE_INIT", "null")), "capabilities": {"general": {"positionEncodings": ["utf-16"]}, "textDocument": {"completion": {"completionItem": {"snippetSupport": False}}, "diagnostic": {}, "publishDiagnostics": {"versionSupport": True}}, "workspace": {"configuration": True}}}, 1)
            initialized = response(1)
            send("initialized", {})
            send("textDocument/didOpen", {"textDocument": {"uri": uri, "languageId": "python" if language == "ruff" else language, "version": 1, "text": initial}})
            send("textDocument/didChange", {"textDocument": {"uri": uri, "version": 2}, "contentChanges": [{"text": edited}]})
            params = {"textDocument": {"uri": uri}, "position": {"line": line, "character": character}}
            if language == "ruff":
                send("textDocument/formatting", {"textDocument": {"uri": uri}, "options": {"tabSize": 4, "insertSpaces": True}}, 2)
                assert response(2) is not None
            else:
                completion = None
                for attempt in range(50):
                    send("textDocument/completion", params, 200 + attempt)
                    try:
                        completion = response(200 + attempt)
                    except ContentModified:
                        completion = None
                    if completion is not None:
                        break
                    time.sleep(0.1)
                assert completion is not None, "server did not answer completion for unsaved document"
                # Indexing can finish after the completion request.
                definition = None
                for attempt in range(10):
                    send("textDocument/definition", params, 10 + attempt)
                    try:
                        definition = response(10 + attempt)
                    except ContentModified:
                        definition = None
                    if definition:
                        break
                    time.sleep(0.25)
                assert definition, "server could not resolve the local definition"
            if initialized["capabilities"].get("diagnosticProvider"):
                send("textDocument/diagnostic", {"textDocument": {"uri": uri}}, 30)
                assert response(30) is not None
            else:
                # Push diagnostics are asynchronous; a successful request is not a flush.
                for attempt in range(50):
                    if diagnostic_messages:
                        break
                    time.sleep(0.1)
                    send("textDocument/hover", params, 100 + attempt)
                    response(100 + attempt)
                assert diagnostic_messages, "server did not publish diagnostics"
            send("textDocument/didClose", {"textDocument": {"uri": uri}})
            send("shutdown", None, 40)
            response(40)
            send("exit", None)
            process.stdin.close()
            process.wait(timeout=10)
            assert process.returncode == 0, process.returncode
            assert source.read_text() == initial, "smoke test unexpectedly changed the file on disk"
            features = "formatting/diagnostics" if language == "ruff" else "completion/definition/diagnostics"
            print(language + ": initialize/open/unsaved edit/" + features + "/close/shutdown passed")
        except Exception:
            sys.stderr.write(peer.stderr_tail())
            raise
        finally:
            peer.kill()
            peer.stderr.close()


if __name__ == "__main__":
    assert len(sys.argv) >= 4 and sys.argv[2] == "--", __doc__
    exercise(sys.argv[1], sys.argv[3:])
