import json
import os
import subprocess
import sys
import tempfile
import time
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
        with self.assertRaisesRegex(SystemExit, "tracking must be"):
            self.load('scene = "a"\nvideo = "b"\ntracking = "both"\n')
        with self.assertRaisesRegex(SystemExit, "tracking_cli need tracking"):
            self.load('scene = "a"\nvideo = "b"\ntracking = "external"\ntracking_cli = "/opt/t"\n')
        with self.assertRaisesRegex(SystemExit, "tracking_worker_args need tracking_cli"):
            self.load('scene = "a"\nvideo = "b"\ntracking_worker_args = ["--x"]\n')
        with self.assertRaisesRegex(SystemExit, "tracking_centre must be"):
            self.load('scene = "a"\nvideo = "b"\ntracking_centre = [1, 2]\n')
        with self.assertRaisesRegex(SystemExit, "tracking_lens must be"):
            self.load('scene = "a"\nvideo = "b"\ntracking_lens = "program"\n')
        with self.assertRaisesRegex(SystemExit, "go together"):
            self.load('scene = "a"\nvideo = "b"\ntracking_lens_k = [-0.05, 0, 0]\n')
        with self.assertRaisesRegex(SystemExit, "go together"):
            self.load('scene = "a"\nvideo = "b"\ntracking_lens_support = [0, 0, 1, 1]\n')
        with self.assertRaisesRegex(SystemExit, "tracking_lens_centre must be 2 numbers"):
            self.load('scene = "a"\nvideo = "b"\ntracking_lens_centre = [1, 2, 3]\n')
        lens = self.load('scene = "a"\nvideo = "b"\ntracking = "external"\ntracking_lens_k = [-0.05, 0, 0]\n'
                         'tracking_lens_centre = [960, 540]\ntracking_lens_scale = 960\n')
        self.assertEqual(lens.tracking_lens_scale, 960, "a CLI lens needs no tracking worker")
        for bad, message in (("program_delay_ms = 1501", "program_delay_ms must be"),
                             ("program_delay_ms = 4", "program_delay_ms must be"),
                             ('input_loss = "freeze"', "input_loss must be"),
                             ('output_rate = "50"', "output_rate must be"),
                             ('audio = "no"', "audio must be")):
            with self.assertRaisesRegex(SystemExit, message):
                self.load(f'scene = "a"\nvideo = "b"\n{bad}\n')

    def test_migrates_the_tracking_wait_to_the_program_delay(self):
        with mock.patch("builtins.print") as printed:
            config = self.load('scene = "a"\nvideo = "b"\ntracking_wait_ms = 800\n')
        self.assertEqual(config.program_delay_ms, 800)
        self.assertIn("tracking_wait_ms is now program_delay_ms", printed.call_args.args[0])
        with self.assertRaisesRegex(SystemExit, "remove tracking_wait_ms"):
            self.load('scene = "a"\nvideo = "b"\ntracking_wait_ms = 800\nprogram_delay_ms = 600\n')


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
        self.assertIn("--tracking-worker", render, "the tracking worker is the default")
        external = stack.Config(scene="s.json", video="v.mp4", tracking="external")
        render = next(c for c in stack.components(external) if c.name == "render").command
        self.assertNotIn("--tracking-worker", render)
        analysis = stack.Config(scene="s.json", video="v.mp4", tracking_cli="/opt/pitch-analysis",
                                tracking_worker_args=["--ledger", "/runs/{stream_id}.arrow"],
                                tracking_centre=[14.5, 60.6, -9.9], program_delay_ms=800,
                                input_loss="slate", output_rate="fixed:50", audio=False)
        render = next(c for c in stack.components(analysis) if c.name == "render").command
        self.assertEqual(render[render.index("--tracking-cli") + 1], "/opt/pitch-analysis")
        self.assertNotIn("--tracking-worker", render, "--tracking-cli implies it in run-tracked.sh")
        args = [render[i + 1] for i, arg in enumerate(render) if arg == "--tracking-worker-arg"]
        self.assertEqual(args, ["--ledger", "/runs/{stream_id}.arrow"])
        centre = render.index("--tracking-centre")
        self.assertEqual(render[centre + 1:centre + 4], ["14.5", "60.6", "-9.9"])
        self.assertEqual(render[render.index("--program-delay-ms") + 1], "800")
        self.assertEqual(render[render.index("--input-loss") + 1], "slate")
        self.assertEqual(render[render.index("--output-rate") + 1], "fixed:50")
        self.assertIn("--no-audio", render)
        self.assertNotIn("--tracking-lens", render, "auto and no CLI lens are chr-server's defaults")
        lens = stack.Config(scene="s.json", video="v.mp4", tracking_lens="pinhole", tracking_lens_k=[-0.05, 0, 0],
                            tracking_lens_centre=[960, 540], tracking_lens_scale=960,
                            tracking_lens_support=[-160, 680, 2080, 1090])
        self.assertEqual(stack.lens_args(lens), [
            "--tracking-lens", "pinhole", "--tracking-lens-k", "-0.05", "0.0", "0.0",
            "--tracking-lens-centre", "960.0", "540.0", "--tracking-lens-scale", "960.0",
            "--tracking-lens-support", "-160.0", "680.0", "2080.0", "1090.0"])
        # A wildcard listener is reached on loopback.
        self.assertIn("srt://127.0.0.1:9001?mode=caller&latency=120000", by_name["srt-relay"].command)


class ReportTests(unittest.TestCase):
    def test_reads_proc_addresses(self):
        self.assertEqual(stack.socket_address("0100007F:2253"), "127.0.0.1:8787")
        self.assertEqual(stack.socket_address("00000000000000000000000001000000:1F90"), "::1:8080")

    def test_short_names(self):
        self.assertEqual(stack.describe(["/x/target/chr-renderer-cli", "worker"]), "chr-renderer-cli worker")
        self.assertIn(stack.describe(["/x/target/chr-tracking-sim-cli", "--manifest", "m.json"]), stack.PORTLESS)
        config = stack.Config(scene="s.json", video="v.mp4", dir="/run/chr")
        self.assertIn("unix /run/chr/capture.sock", stack.roles(config, False))
        self.assertIn(stack.describe(["/nix/store/x/bin/pitch-analysis", "--input-socket", "a"]), stack.PORTLESS)
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


class HealthTests(unittest.TestCase):
    def test_calls_chr_server_over_its_native_socket(self):
        import socket as sockets
        import threading
        with tempfile.TemporaryDirectory() as directory:
            path = f"{directory}/render.sock"
            listener = sockets.socket(sockets.AF_UNIX)
            listener.bind(path)
            listener.listen()

            def serve():
                connection, _ = listener.accept()
                with connection:
                    for _ in range(2):
                        request = json.loads(stack.receive(connection, int.from_bytes(stack.receive(connection, 4), "big")))
                        replies = [{"jsonrpc": "2.0", "method": "note"}]  # not an answer; skipped
                        if request["method"] == "sdi.get_status":
                            replies.append({"jsonrpc": "2.0", "id": request["id"], "result": {"frames_output": 7}})
                        else:
                            replies.append({"jsonrpc": "2.0", "id": request["id"], "error": {"code": -32601, "message": "Method not found"}})
                        for reply in replies:
                            payload = json.dumps(reply).encode()
                            connection.sendall(len(payload).to_bytes(4, "big") + payload)

            thread = threading.Thread(target=serve)
            thread.start()
            results = stack.rpc(path, ["sdi.get_status", "tracking.get_worker_status"])
            thread.join()
            listener.close()
        self.assertEqual(results, {"sdi.get_status": {"frames_output": 7},
                                   "tracking.get_worker_status": {"error": "Method not found"}})

    def test_reports_rates_and_problems(self):
        sdi = {"video_mode": "1280x720p30", "frames_output": 100, "repeated_frames": 2, "dropped_frames": 0}
        render = {"state": "ready", "pid": 5, "restarts": 0, "deadline_misses": 1, "clean_feed_frames": 0, "last_error": None}
        tracker = {"state": "backoff", "worker": "pitch-analysis", "stream_id": None, "pid": None, "restarts": 1,
                   "requests": 50, "cameras": 49, "unavailable": 0, "not_sent": 3,
                   "last_error": "the tracking worker exited"}
        join = {"frames_joined": 40, "frames_without_tracking": 3, "samples_late": 0,
                "poses_fresh": 13, "poses_interpolated": 26, "poses_reused": 10, "poses_unavailable": 1,
                "poses_overdue": 2, "camera_model": {"model": "radial+warp", "source": "worker"}}
        first = {"sdi.get_status": sdi, "render.get_worker_status": render, "tracking.get_worker_status": tracker,
                 "tracking.get_status": join}
        second = {**first, "sdi.get_status": {**sdi, "frames_output": 130, "repeated_frames": 12},
                  "tracking.get_status": {**join, "frames_without_tracking": 13}}
        text = "\n".join(stack.health(first, second, 1.0))
        self.assertIn("30.0 slots/s, 10.0 repeats/s", text)
        self.assertNotIn("program (core)", text, "a generated program has no core")
        self.assertIn("PROBLEM: tracking worker backoff: the tracking worker exited", text)
        self.assertIn("tracking worker   pitch-analysis: backoff", text)
        self.assertIn("13 fresh, 26 interpolated, 10 reused, 1 unavailable, 2 overdue", text)
        self.assertIn("camera model      radial+warp (lens source: worker)", text)
        self.assertIn("PROBLEM: no frame was paired with a camera", text)
        spread = {"min": 20.5, "mean": 60.25, "max": 90.0, "count": 280}
        core = {"program_delay_ms": 800.0, "decision_margin_ms": 4.0, "cg_slots": 900, "clean_slots": 100,
                "empty_slots": 0, "late_cg": 2, "late_decisions": 0, "cg_ratio": 0.9,
                "window": {"seconds": 10.0, "cg_slots": 120, "clean_slots": 180, "cg_ratio": 0.4,
                           "slack_ms": spread, "render_budget_ms": spread, "delay_ms": None}}
        cored = {**second, "sdi.get_status": {**second["sdi.get_status"], "core": core, "input_loss_slots": 5,
                                              "input_losses": 1}}
        text = "\n".join(stack.health(first, cored, 1.0))
        self.assertIn("program (core)    D 800 ms, decided 4 ms before due; CG 90.0 % since start, 40.0 %", text)
        self.assertIn("900 CG, 100 clean feed, 0 empty (repeated), 2 late CG", text)
        self.assertIn("input loss 5 slots in 1 losses", text)
        self.assertIn("slack of CG slots 20.5/60.2/90.0 ms", text)
        self.assertIn("PROBLEM: only 40.0 % of the slots had CG", text)
        external = {**second, "tracking.get_worker_status": {"error": "Method not found"}}
        self.assertIn("none: cameras come from tracking.frame", "\n".join(stack.health(first, external, 1.0)))


class StartTests(unittest.TestCase):
    def test_names_the_process_holding_a_port(self):
        import socket as sockets
        with sockets.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            listener.listen()
            port = listener.getsockname()[1]
            self.assertTrue(stack.holder("127.0.0.1", port).startswith(f"pid {os.getpid()} ("))

    def test_waits_for_ready_workers_and_flowing_frames(self):
        def sample(state, frames):
            worker = {"state": state, "pid": 7, "restarts": 0, "last_error": None}
            return {"sdi.get_status": {"frames_output": frames}, "render.get_worker_status": worker,
                    "tracking.get_worker_status": worker, "tracking.get_status": {"frames_joined": frames}}
        samples = iter([sample("starting", 0), sample("ready", 0), sample("ready", 30), sample("ready", 60)])
        config = stack.Config(scene="s.json", video="v.mp4")
        with mock.patch.object(stack, "rpc", side_effect=lambda *_: next(samples)), \
                mock.patch.object(stack.time, "sleep"), mock.patch("builtins.print") as printed:
            self.assertEqual(stack.wait_healthy(config, time.monotonic() + 60), [])
        lines = [call.args[0] for call in printed.call_args_list]
        self.assertEqual(lines, ["ready: render worker (pid 7)", "ready: tracking worker (pid 7)",
                                 "ready: video flowing, 30 SDI slots/s, 30 frames/s paired with a camera"])

    def test_reports_what_is_still_wrong_at_the_deadline(self):
        stuck = {"render.get_worker_status": {"state": "backoff", "pid": None, "restarts": 3, "last_error": "device lost"}}
        config = stack.Config(scene="s.json", video="v.mp4")
        with mock.patch.object(stack, "rpc", return_value=stuck), mock.patch.object(stack.time, "sleep"):
            self.assertEqual(stack.wait_healthy(config, time.monotonic() + 0.05), ["render worker is backoff: device lost"])


class StopTests(unittest.TestCase):
    def start(self, directory: str, name: str, script: str) -> subprocess.Popen:
        process = subprocess.Popen(["sh", "-c", script], start_new_session=True)
        time.sleep(0.2)
        (Path(directory) / f"{name}.pid").write_text(f"{process.pid} {stack.start_time(process.pid)}\n")
        return process

    def test_lists_every_stopped_and_killed_process(self):
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(stack, "STATE", Path(directory)), \
                mock.patch.object(stack, "STOP_TIMEOUT_S", 1):
            polite = self.start(directory, "polite", "sleep 30 & wait")
            stubborn = self.start(directory, "stubborn", "trap '' TERM; sleep 30; true")
            try:
                with mock.patch("builtins.print"):
                    stopped = stack.stop()
            finally:
                for process in (polite, stubborn):
                    process.kill()
                    process.wait()
            by_component = {}
            for component, pid, name, outcome in stopped:
                by_component.setdefault(component, []).append((name.split()[0], outcome))
            self.assertEqual(sorted(by_component["polite"]), [("sh", "stopped (SIGTERM)"), ("sleep", "stopped (SIGTERM)")])
            self.assertEqual(sorted(by_component["stubborn"]), [("sh", "killed (SIGKILL)"), ("sleep", "killed (SIGKILL)")])
            self.assertEqual(list(Path(directory).glob("*.pid")), [])

    def test_stops_only_the_named_components(self):
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(stack, "STATE", Path(directory)):
            kept = self.start(directory, "kept", "sleep 30")
            gone = self.start(directory, "gone", "sleep 30")
            try:
                with mock.patch("builtins.print"):
                    stopped = stack.stop(["gone"])
                self.assertEqual({component for component, *_ in stopped}, {"gone"})
                self.assertEqual(stack.running("kept"), kept.pid)
                self.assertEqual([path.name for path in Path(directory).glob("*.pid")], ["kept.pid"])
            finally:
                for process in (kept, gone):
                    process.kill()
                    process.wait()


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
