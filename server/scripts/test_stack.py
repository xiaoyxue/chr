import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import stack  # noqa: E402


class ConfigTests(unittest.TestCase):
    def load(self, text: str) -> stack.Config:
        with tempfile.NamedTemporaryFile("w", suffix=".toml", delete=False) as file:
            file.write(text)
        try:
            return stack.load_config(Path(file.name))
        finally:
            os.unlink(file.name)

    def test_example_config_loads(self):
        example = Path(stack.__file__).with_name("stack.example.toml")
        config = stack.load_config(example)
        self.assertEqual(config.pitch_studio_port, 4173)
        self.assertEqual(config.srt_relay_port, 0)

    def test_rejects_unknown_and_missing_keys(self):
        with self.assertRaisesRegex(SystemExit, "unknown keys sene"):
            self.load('scene = "a"\nvideo = "b"\nsene = "c"\n')
        with self.assertRaisesRegex(SystemExit, "missing video"):
            self.load('scene = "a"\n')


class ComponentTests(unittest.TestCase):
    def test_optional_components_and_render_arguments(self):
        config = stack.Config(scene="s.json", video="v.mp4", nixgl=False, pitch_studio_port=0, srt_relay_port=18991,
                              srt="0.0.0.0:9001", asset_root="/assets")
        by_name = {component.name: component for component in stack.components(config)}
        self.assertEqual(sorted(by_name), ["monitor-page", "render", "srt-relay"])
        render = by_name["render"].command
        self.assertNotIn("github:nix-community/nixGL#nixGLNvidia", render)
        self.assertEqual(render[render.index("--asset-root") + 1], "/assets")
        self.assertNotIn("--description", render)
        # A wildcard listener is reached on loopback.
        self.assertIn("srt://127.0.0.1:9001?mode=caller&latency=120000", by_name["srt-relay"].command)


class ReportTests(unittest.TestCase):
    def test_reads_proc_addresses(self):
        self.assertEqual(stack.socket_address("0100007F:2253"), "127.0.0.1:8787")
        self.assertEqual(stack.socket_address("00000000000000000000000001000000:1F90"), "::1:8080")

    def test_short_names(self):
        self.assertEqual(stack.describe(["/x/target/chr-renderer-cli", "worker"]), "chr-renderer-cli worker")
        self.assertEqual(stack.describe(["bash", "/x/server/scripts/run-tracked.sh", "--scene", "s.json"]), "bash run-tracked.sh")
        self.assertEqual(stack.describe(["python3", "server/scripts/stack.py", "relay", "srt://a:1"]), "python3 stack.py relay")
        self.assertEqual(stack.describe(["npm exec vite --host 127.0.0.1"]), "npm exec vite")

    def test_finds_the_ports_a_process_group_holds(self):
        child = subprocess.Popen(
            [sys.executable, "-c", "import socket, time\ns = socket.socket(); s.bind(('127.0.0.1', 0)); s.listen()\n"
             "print(s.getsockname()[1], flush=True); time.sleep(30)"],
            stdout=subprocess.PIPE, text=True, start_new_session=True)
        try:
            port = int(child.stdout.readline())
            self.assertEqual(stack.group_processes(child.pid)[0][0], child.pid)
            self.assertIn(f"tcp 127.0.0.1:{port}", stack.process_sockets(child.pid, stack.listening_sockets()))
        finally:
            child.kill()
            child.wait()
            child.stdout.close()

    def test_forwards_skip_disabled_ports(self):
        config = stack.Config(scene="s.json", video="v.mp4", server_ws="127.0.0.1:18987", monitor_ws="127.0.0.1:18989",
                              webrtc_tcp="127.0.0.1:18992", srt_relay_port=18991, monitor_page_port=0)
        self.assertEqual(stack.forwarded_ports(config), [4173, 18987, 18989, 18992, 18993, 18994, 18995, 18991])


class RecordTests(unittest.TestCase):
    def test_a_reused_pid_is_not_ours(self):
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(stack, "STATE", Path(directory)):
            child = subprocess.Popen(["sleep", "30"])
            try:
                record = Path(directory) / "x.pid"
                record.write_text(f"{child.pid} {stack.start_time(child.pid)}\n")
                self.assertEqual(stack.running("x"), child.pid)
                record.write_text(f"{child.pid} 1\n")
                self.assertIsNone(stack.running("x"))
                self.assertFalse(record.exists())
            finally:
                child.kill()
                child.wait()


if __name__ == "__main__":
    unittest.main()
