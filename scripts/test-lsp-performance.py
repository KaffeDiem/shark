"""Offline checks for the manual performance harness, without starting language tools."""

import importlib.util
import os
import pathlib
import queue
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "lsp_performance", pathlib.Path(__file__).with_name("lsp-performance.py")
)
benchmark = importlib.util.module_from_spec(spec)
spec.loader.exec_module(benchmark)


class PerformanceHarnessTests(unittest.TestCase):
    def test_notifications_cannot_extend_a_request_deadline(self):
        peer = benchmark.Peer.__new__(benchmark.Peer)
        peer.ident = 0
        peer.progress = {}
        peer.messages = queue.Queue()
        peer.messages.put({"method": "window/logMessage", "params": {}})
        peer.write = lambda _: None
        with patch.object(benchmark.time, "monotonic", side_effect=[0, 0, 91]):
            with self.assertRaises(TimeoutError):
                peer.request("textDocument/definition", {})

    def test_failed_memory_sampling_fails_and_closes_both_peers(self):
        failed = threading.Event()
        closed = []

        def fail_sampling(_):
            if threading.current_thread() is threading.main_thread():
                return 1
            failed.set()
            raise OSError("ps unavailable")

        class FakePeer:
            def __init__(self, *_):
                self.process = type("Process", (), {"pid": 123})()
                self.progress = {}
                assert failed.wait(1)

            def request(self, method, *_):
                return (
                    [{"label": "answer"}]
                    if method.endswith("completion")
                    else {"uri": "file:///mod_0000.py"}
                )

            def notify(self, *_):
                pass

            def close(self):
                closed.append(self)

        with (
            patch.object(benchmark, "Peer", FakePeer),
            patch.object(benchmark, "artifact", return_value="unused"),
            patch.object(benchmark, "rss_tree", side_effect=fail_sampling),
            patch.object(benchmark.time, "sleep"),
        ):
            with self.assertRaisesRegex(AssertionError, "ps unavailable"):
                benchmark.run(1)
        self.assertEqual(len(closed), 2)

    def test_kill_reaps_children_of_an_exited_server_wrapper(self):
        with tempfile.TemporaryDirectory() as root:
            peer = benchmark.jsonrpc_peer.Peer(
                ["/bin/sh", "-c", "sleep 60 & echo $! > child"], root
            )
            peer.process.wait()
            child = int(pathlib.Path(root, "child").read_text())
            peer.kill()
            peer.stderr.close()
            deadline = time.monotonic() + 5
            while True:
                try:
                    os.kill(child, 0)
                except ProcessLookupError:
                    break
                self.assertLess(time.monotonic(), deadline, "child server survived")
                time.sleep(0.05)

    def test_kill_accepts_a_server_that_already_exited(self):
        with tempfile.TemporaryDirectory() as root:
            peer = benchmark.jsonrpc_peer.Peer(["/usr/bin/true"], root)
            time.sleep(0.2)
            peer.kill()
            peer.stderr.close()
            self.assertEqual(peer.process.returncode, 0)


if __name__ == "__main__":
    unittest.main()
