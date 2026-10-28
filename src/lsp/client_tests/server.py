"""Controllable stdio LSP peer. Never prints non-protocol text to stdout."""
import json
import os
import sys
import time

log_path = sys.argv[1]
documents = {}
waiting = []
ignore_shutdown = False


def send(value):
    encoded = json.dumps(dict(jsonrpc="2.0", **value)).encode()
    sys.stdout.buffer.write(f"Content-Length: {len(encoded)}\r\n\r\n".encode() + encoded)
    sys.stdout.buffer.flush()


def respond(message, result):
    send(dict(id=message["id"], result=result))


with open(log_path, "a") as log:
    log.write(json.dumps(dict(pid=os.getpid())) + "\n")
    log.flush()
    while True:
        header = sys.stdin.buffer.readline()
        if not header:
            break
        length = int(header.decode().split(":")[1])
        assert sys.stdin.buffer.readline() == b"\r\n"
        message = json.loads(sys.stdin.buffer.read(length))
        log.write(json.dumps(message) + "\n")
        log.flush()
        method = message.get("method")
        params = message.get("params", {})
        if method == "initialize":
            if (params.get("initializationOptions") or {}).get("testProgress"):
                send(dict(method="$/progress", params=dict(token="index", value=dict(kind="begin", title="Index"))))
            respond(message, dict(capabilities=dict(positionEncoding="utf-16", textDocumentSync=dict(openClose=True, change=1, save=dict(includeText=True)), completionProvider=dict(resolveProvider=True))))
        elif method == "workspace/didChangeConfiguration":
            send(dict(id="config", method="workspace/configuration", params=dict(items=[dict(section="python.analysis"), {}])))
        elif method == "textDocument/didOpen":
            item = params["textDocument"]
            documents[item["uri"]] = item["text"]
        elif method == "textDocument/didChange":
            item = params["textDocument"]
            documents[item["uri"]] = params["contentChanges"][0]["text"]
        elif method == "textDocument/completion":
            text = documents[params["textDocument"]["uri"]]
            if text.startswith("# resolve-completion"):
                respond(message, [dict(label="sqrt", data=dict(token="math.sqrt"), textEdit=dict(range=dict(start=dict(line=1, character=0), end=dict(line=1, character=2)), newText="sqrt"))])
            else:
                respond(message, text)
            send(dict(method="textDocument/publishDiagnostics", params=dict(uri=params["textDocument"]["uri"], version=2, diagnostics=[])))
        elif method == "completionItem/resolve":
            assert params["data"]["token"] == "math.sqrt"
            params["additionalTextEdits"] = [dict(range=dict(start=dict(line=0, character=0), end=dict(line=0, character=0)), newText="from math import sqrt\n")]
            respond(message, params)
        elif method == "test/applyEdit":
            send(dict(id="apply", method="workspace/applyEdit", params=dict(edit=dict(changes={}))))
            respond(message, None)
        elif method == "test/endProgress":
            send(dict(method="$/progress", params=dict(token="index", value=dict(kind="end"))))
            respond(message, None)
        elif method == "test/wait":
            waiting.append(message)
        elif method == "test/ignoreShutdown":
            ignore_shutdown = True
            respond(message, None)
        elif method == "test/crash":
            sys.exit(2)
        elif method == "shutdown":
            if not ignore_shutdown:
                respond(message, None)
        elif method == "exit":
            break
        elif method and "id" in message:
            for pending in waiting:
                respond(pending, "obsolete")
            waiting.clear()
            respond(message, params)
