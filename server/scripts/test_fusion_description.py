import hashlib
import io
import json
import math
import struct
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import fusion_description  # noqa: E402

GLB_ID, PNG_ID = "ASSET-model", "ASSET-image"


def glb(nodes, accessor):
    document = json.dumps({"asset": {"version": "2.0"}, "scene": 0, "scenes": [{"nodes": [0]}], "nodes": nodes,
                           "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
                           "accessors": [accessor], "materials": [{"name": "glass", "alphaMode": "BLEND"}]}).encode()
    document += b" " * (-len(document) % 4)
    return b"glTF" + struct.pack("<III", 2, 20 + len(document), len(document)) + b"JSON" + document


def png(width, height):
    return b"\x89PNG\r\n\x1a\n" + struct.pack(">I", 13) + b"IHDR" + struct.pack(">II", width, height) + bytes(5)


def package(model, image, pose=None):
    contents = {f"assets/{GLB_ID}.glb": model, f"assets/{PNG_ID}.png": image}
    placement = {"coordinateSpace": "pitch", "x": 1, "y": 2, "width": 10, "height": 4}
    match = {
        "schemaVersion": 1, "sport": "football", "field": {"length": 105, "width": 68},
        "coordinates": {"unit": "meters", "origin": "field-center", "x": "right",
                        "y": "down-in-top-view", "modelUp": "gltf-y-up"},
        "ads": [
            {"id": "AD-3D", "type": "3D", "placements": [{**placement, "id": "PLC-model", "assetIds": [GLB_ID],
             "modelPose": pose or {"x": 0, "y": 0, "scale": 0.5, "yaw": 90}}]},
            {"id": "AD-2D", "type": "2D", "placements": [{**placement, "id": "PLC-image", "assetIds": [PNG_ID],
             "assetRotation": 90}]},
        ],
        "assets": [{"id": asset, "contentType": kind, "contentPath": entry, "byteLength": len(contents[entry])}
                   for asset, kind, entry in ((GLB_ID, "model/gltf-binary", f"assets/{GLB_ID}.glb"),
                                              (PNG_ID, "image/png", f"assets/{PNG_ID}.png"))],
    }
    manifest = {"packageVersion": 1, "match": match,
                "sha256": {entry: hashlib.sha256(data).hexdigest() for entry, data in contents.items()}}
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as archive:
        archive.writestr("manifest.json", json.dumps(manifest))
        for entry, data in contents.items():
            archive.writestr(entry, data)
    return buffer.getvalue()


class DescribeSceneTests(unittest.TestCase):
    def describe(self, model, pose=None, **options):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "match.zip"
            path.write_bytes(package(model, png(200, 100), pose))
            return fusion_description.describe_scene(path, "/srv/match.zip", **options)

    def test_glb_bounds_follow_node_transforms_like_three(self):
        # Scaled by 2 and moved by (1, 3, 0): glTF bounds x [1, 5], y [1, 3], z [-2, 6].
        model = glb([{"mesh": 0, "translation": [1, 3, 0], "scale": [2, 2, 2]}],
                    {"componentType": 5126, "min": [0, -1, -1], "max": [2, 0, 3]})
        scene = self.describe(model)
        glb_model = scene["instances"][0]["model"]
        # Footprint 4 x 8 turned by 90 degrees fits 10 x 4 at scale min(10/8, 4/4) * 0.5.
        self.assertAlmostEqual(glb_model[2][2], 0.5)
        expected = [[0, -0.5, 0, 1 + 0.5 * 2], [0.5, 0, 0, 2 - 0.5 * 3], [0, 0, 0.5, -0.015 + 0.5 * 1], [0, 0, 0, 1]]
        for row, want in zip(glb_model, expected):
            for value, target in zip(row, want):
                self.assertAlmostEqual(value, target)
        self.assertEqual(scene["assets"][0]["blend_as"], "mask")
        self.assertEqual(scene["assets"][0]["source"], {"package": "/srv/match.zip", "entry": f"assets/{GLB_ID}.glb"})

    def test_rotated_image_is_contain_fitted(self):
        model = glb([{"mesh": 0}], {"componentType": 5126, "min": [-1, 0, -1], "max": [1, 1, 1]})
        quad = self.describe(model)["instances"][1]["model"]
        # Rotated by 90 degrees, the 200 x 100 image fits 10 x 4 at scale min(10/100, 4/200).
        self.assertAlmostEqual(quad[1][0], 4)
        self.assertAlmostEqual(quad[0][1], -2)
        self.assertTrue(math.isclose(quad[0][0], 0, abs_tol=1e-12))

    def test_move_and_hide_apply_to_named_placements(self):
        model = glb([{"mesh": 0}], {"componentType": 5126, "min": [-1, 0, -1], "max": [1, 1, 1]})
        base = self.describe(model)
        moved = self.describe(model, moves=[("PLC-model", 0, 10)], hidden=["PLC-image"])
        self.assertAlmostEqual(moved["instances"][0]["model"][1][3] - base["instances"][0]["model"][1][3], 10)
        self.assertEqual(moved["instances"][0]["model"][0][3], base["instances"][0]["model"][0][3])
        self.assertFalse(moved["instances"][1]["visible"])
        with self.assertRaises(fusion_description.ConversionError):
            self.describe(model, moves=[("PLC-missing", 0, 1)])

    def test_scale_resizes_models_and_image_areas(self):
        model = glb([{"mesh": 0}], {"componentType": 5126, "min": [-1, 0, -1], "max": [1, 1, 1]})
        base = self.describe(model)["instances"]
        scaled = self.describe(model, scales=[("PLC-model", 0.5), ("PLC-image", 1.25)])["instances"]
        self.assertAlmostEqual(scaled[0]["model"][2][2], base[0]["model"][2][2] * 0.5)
        self.assertAlmostEqual(scaled[1]["model"][1][0], base[1]["model"][1][0] * 1.25)
        self.assertEqual(scaled[1]["model"][0][3], base[1]["model"][0][3])

    def test_rotate_turns_models_without_refitting(self):
        # A 4 x 2 footprint: refitting at 90 degrees would shrink it to fit the 10 x 4 region.
        model = glb([{"mesh": 0}], {"componentType": 5126, "min": [-2, 0, -1], "max": [2, 1, 1]})
        base = self.describe(model)["instances"][0]["model"]
        turned = self.describe(model, rotations=[("PLC-model", 90)])["instances"][0]["model"]
        self.assertAlmostEqual(turned[2][2], base[2][2])
        self.assertAlmostEqual(turned[0][0], -base[1][0])
        self.assertAlmostEqual(turned[1][0], base[0][0])

    def test_model_animation_becomes_motion_about_the_bottom_centre(self):
        model = glb([{"mesh": 0}], {"componentType": 5126, "min": [-1, 0, -1], "max": [1, 1, 1]})
        self.assertNotIn("motion", self.describe(model)["instances"][0])
        pose = {"x": 0, "y": 0, "scale": 0.5, "yaw": 90, "swingAxis": "z", "swingAmplitude": 45,
                "swingPeriod": 2, "rotationMode": "continuous",
                "motion": {"enabled": True, "start": {"x": -8, "y": 2, "z": 0, "scale": 0.5},
                           "end": {"x": 16, "y": -6, "z": -4, "scale": 1.5}, "duration": 4, "playback": "ping-pong"}}
        motion = self.describe(model, pose)["instances"][0]["motion"]
        self.assertEqual(motion["pivot"], [1, 2, -0.015])
        self.assertEqual(motion["rotation"], {"axis": [0, 0, 1], "mode": "continuous",
                                              "amplitude": math.pi / 4, "period": 2})
        self.assertEqual(motion["path"], {"start": {"offset": [-8, 2, 0], "scale": 0.5},
                                          "end": {"offset": [16, -6, -4], "scale": 1.5},
                                          "duration": 4, "playback": "ping_pong"})
        pose["motion"]["enabled"] = False
        pose["swingAxis"] = None
        self.assertNotIn("motion", self.describe(model, pose)["instances"][0])


if __name__ == "__main__":
    unittest.main()
