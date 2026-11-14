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


class SummaryTests(unittest.TestCase):
    def test_lists_every_process_address_and_forward(self):
        config = stack.Config(scene="s.json", video="v.mp4", server_ws="127.0.0.1:18987", monitor_ws="127.0.0.1:18989",
                              webrtc_tcp="127.0.0.1:18992", srt="10.0.0.5:18990", srt_relay_port=18991)
        text = stack.summary(config, probe=False)
        self.assertIn("4 components (monitor-page, pitch-studio, render, srt-relay), 8 working processes", text)
        for address in ["ws://127.0.0.1:18987", "ws://127.0.0.1:18989", "srt://10.0.0.5:18990",
                        "tcp://127.0.0.1:18992-18995", "tcp://127.0.0.1:18991", "http://127.0.0.1:4173/", "http://127.0.0.1:5173/"]:
            self.assertIn(address, text)
        self.assertEqual(stack.forwarded_ports(config), [4173, 5173, 18987, 18989, 18992, 18993, 18994, 18995, 18991])

    def test_disabled_components_are_left_out(self):
        config = stack.Config(scene="s.json", video="v.mp4", pitch_studio_port=0, monitor_page_port=0)
        self.assertIn("1 components (render), 5 working processes", stack.summary(config, probe=False))
        self.assertNotIn(0, stack.forwarded_ports(config))


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
