#!/usr/bin/env python3
"""Verify SweetPad's shipped BSP against a small real Xcode project, without building."""
import json
import os
import pathlib
import queue
import signal
import subprocess
import sys
import tempfile
import threading
import time

PROJECT = '''// !$*UTF8*$!
{ archiveVersion = 1; classes = {}; objectVersion = 56;
objects = {
 A00000000000000000000001 = {isa = PBXProject; buildConfigurationList = A00000000000000000000002; compatibilityVersion = "Xcode 14.0"; developmentRegion = en; knownRegions = (en, Base); mainGroup = A00000000000000000000003; productRefGroup = A00000000000000000000004; projectDirPath = ""; projectRoot = ""; targets = (A00000000000000000000005); };
 A00000000000000000000002 = {isa = XCConfigurationList; buildConfigurations = (A00000000000000000000006); defaultConfigurationIsVisible = 0; defaultConfigurationName = Debug; };
 A00000000000000000000003 = {isa = PBXGroup; children = (A00000000000000000000007, A00000000000000000000004); sourceTree = "<group>"; };
 A00000000000000000000004 = {isa = PBXGroup; children = (A00000000000000000000008); name = Products; sourceTree = "<group>"; };
 A00000000000000000000005 = {isa = PBXNativeTarget; buildConfigurationList = A00000000000000000000009; buildPhases = (A00000000000000000000010); buildRules = (); dependencies = (); name = App; productName = App; productReference = A00000000000000000000008; productType = "com.apple.product-type.tool"; };
 A00000000000000000000006 = {isa = XCBuildConfiguration; buildSettings = {SDKROOT = macosx; SWIFT_VERSION = 6.0; MACOSX_DEPLOYMENT_TARGET = 14.0; }; name = Debug; };
 A00000000000000000000007 = {isa = PBXFileReference; lastKnownFileType = sourcecode.swift; path = main.swift; sourceTree = "<group>"; };
 A00000000000000000000008 = {isa = PBXFileReference; explicitFileType = "compiled.mach-o.executable"; includeInIndex = 0; path = App; sourceTree = BUILT_PRODUCTS_DIR; };
 A00000000000000000000009 = {isa = XCConfigurationList; buildConfigurations = (A00000000000000000000011); defaultConfigurationIsVisible = 0; defaultConfigurationName = Debug; };
 A00000000000000000000010 = {isa = PBXSourcesBuildPhase; buildActionMask = 2147483647; files = (A00000000000000000000012); runOnlyForDeploymentPostprocessing = 0; };
 A00000000000000000000011 = {isa = XCBuildConfiguration; buildSettings = {PRODUCT_NAME = "$(TARGET_NAME)"; SWIFT_VERSION = 6.0; CODE_SIGNING_ALLOWED = NO; }; name = Debug; };
 A00000000000000000000012 = {isa = PBXBuildFile; fileRef = A00000000000000000000007; };
}; rootObject = A00000000000000000000001; }
'''


def exercise(program):
    with tempfile.TemporaryDirectory(prefix="shark-bsp-proof-") as temp:
        root = pathlib.Path(temp)
        project = root / "App.xcodeproj"
        project.mkdir()
        (project / "project.pbxproj").write_text(PROJECT)
        (root / "main.swift").write_text('print("Shark BSP proof")\n')
        subprocess.run([program, "--non-interactive", "bsp", "init", "--project", str(project)], cwd=root, check=True, timeout=30)
        config = json.loads((root / "buildServer.json").read_text())
        assert all(key in config for key in ["name", "version", "bspVersion", "languages", "argv"])
        with tempfile.TemporaryFile() as stderr:
            child = subprocess.Popen(config["argv"], cwd=root, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=stderr, start_new_session=True)
            lifecycle_deadline = time.monotonic() + 120
            messages = queue.Queue()

            def read():
                try:
                    while True:
                        length = None
                        while True:
                            header = child.stdout.readline()
                            if not header:
                                raise EOFError("BSP server exited")
                            if header in (b"\r\n", b"\n"):
                                break
                            if header.lower().startswith(b"content-length:"):
                                length = int(header.split(b":", 1)[1])
                        messages.put(json.loads(child.stdout.read(length)))
                except Exception as error:
                    messages.put(error)

            threading.Thread(target=read, daemon=True).start()

            def send(method, params=None, ident=None):
                message = {"jsonrpc": "2.0", "method": method}
                if params is not None:
                    message["params"] = params
                if ident is not None:
                    message["id"] = ident
                raw = json.dumps(message).encode()
                child.stdin.write(("Content-Length: %d\r\n\r\n" % len(raw)).encode() + raw)
                child.stdin.flush()

            def response(ident):
                deadline = min(time.monotonic() + 60, lifecycle_deadline)
                while time.monotonic() < deadline:
                    message = messages.get(timeout=max(0.01, deadline - time.monotonic()))
                    if isinstance(message, Exception):
                        raise message
                    if message.get("id") == ident:
                        assert "error" not in message, message
                        return message["result"]
                raise TimeoutError("BSP response timed out")

            try:
                send("build/initialize", {"displayName": "Shark release gate", "version": "1", "bspVersion": "2.2.0", "rootUri": root.as_uri(), "capabilities": {"languageIds": ["swift"]}}, 1)
                assert response(1)["capabilities"] is not None
                send("build/initialized", {})
                send("workspace/buildTargets", {}, 2)
                assert response(2)["targets"], "BSP returned no targets for the Xcode fixture"
                send("build/shutdown", None, 3)
                response(3)
                send("build/exit")
                child.wait(timeout=10)
                assert child.returncode == 0, child.returncode
                print("SweetPad BSP: generate/initialize/buildTargets/shutdown passed")
            except Exception:
                stderr.seek(0)
                sys.stderr.write(stderr.read().decode(errors="replace")[-8000:])
                raise
            finally:
                if child.poll() is None:
                    os.killpg(child.pid, signal.SIGKILL)
                    child.wait()


if __name__ == "__main__":
    exercise(sys.argv[1])
