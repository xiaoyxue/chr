import hashlib
from pathlib import Path
import subprocess
import tempfile
import unittest

from mp4_gstreamer_source import receive


class SoftwareFeedTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temporary = tempfile.TemporaryDirectory()
        cls.source = Path(cls.temporary.name) / "synthetic.mp4"
        subprocess.run(
            [
                "ffmpeg", "-nostdin", "-hide_banner", "-loglevel", "error",
                "-f", "lavfi", "-i", "testsrc2=size=64x48:rate=30000/1001",
                "-vf", "setparams=range=limited:color_primaries=bt709:"
                "color_trc=bt709:colorspace=bt709",
                "-frames:v", "4", "-c:v", "libx264", "-threads", "1",
                "-pix_fmt", "yuv420p", "-color_range", "tv",
                "-colorspace", "bt709", "-color_trc", "bt709",
                "-color_primaries", "bt709", str(cls.source),
            ],
            check=True,
        )

    @classmethod
    def tearDownClass(cls):
        cls.temporary.cleanup()

    def test_decoded_pixels_reach_appsink_and_eof(self):
        result = receive(self.source, realtime=False)
        reference = subprocess.run(
            [
                "ffmpeg", "-nostdin", "-hide_banner", "-loglevel", "error",
                "-i", str(self.source), "-frames:v", "1",
                "-vf", "scale=in_range=tv:out_range=tv:"
                "in_color_matrix=bt709:out_color_matrix=bt709,format=uyvy422",
                "-threads", "1", "-f", "rawvideo", "pipe:1",
            ],
            check=True, capture_output=True,
        ).stdout
        self.assertEqual(result["frames"], 4)
        self.assertEqual(result["bytes"], 4 * 64 * 48 * 2)
        self.assertEqual(result["first_sha256"], hashlib.sha256(reference).hexdigest())
        self.assertAlmostEqual(result["last_pts_ns"], 100100000, delta=4)
        self.assertNotEqual(result["first_sha256"], result["last_sha256"])

    def test_loop_crosses_eof_without_restarting_software_time(self):
        result = receive(self.source, frames=9, loop=True, realtime=False)
        self.assertEqual(result["frames"], 9)
        self.assertAlmostEqual(
            result["last_pts_ns"], 8 * 1000000000 * 1001 // 30000, delta=9
        )
        self.assertEqual(result["first_sha256"], result["last_sha256"])

    def test_short_source_does_not_claim_requested_frames(self):
        with self.assertRaisesRegex(RuntimeError, "expected 9 frames, received 4"):
            receive(self.source, frames=9, realtime=False)


if __name__ == "__main__":
    unittest.main()
