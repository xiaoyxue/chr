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


def package(model, image, pose=None, opacity=None):
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
             "assetRotation": 90, **({} if opacity is None else {"assetOpacity": opacity})}]},
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


CORNERS = [(10, 40, -6), (-10, 40, -6), (-10, 44, -2), (10, 44, -2)]


CENTRE_REVEAL = {"direction": "center-horizontal", "duration": 2}
ABSENT = object()


def tifo_package(corners=CORNERS, opacity=1, cloud=b"ply\nformat ascii 1.0\n", cloud_hash=None, reveal=CENTRE_REVEAL):
    """A package v2 holding one Tifo on a raked plane, 20 m wide and 4 * sqrt(2) m high."""
    image = png(960, 360)
    contents = {f"assets/{PNG_ID}.png": image, "point-cloud/field.ply": cloud}
    match = {
        "schemaVersion": 2, "sport": "football",
        "field": {"length": 105, "width": 68, "pointCloud": {"fileName": "field.ply", "byteLength": len(cloud),
                                                            "contentPath": "point-cloud/field.ply"}},
        "coordinates": {"unit": "meters", "origin": "field-center", "x": "right", "y": "down-in-top-view",
                        "modelUp": "gltf-y-up", **fusion_description.V2_COORDINATES},
        "ads": [{"id": "AD-tifo", "type": "Tifo", "placements": [{
            "id": "PLC-tifo", "coordinateSpace": "point-cloud", "x": 0, "y": 42, "width": 20,
            "height": 4 * math.sqrt(2), "assetRotation": 90, "assetOpacity": opacity, "modelPose": None,
            "assetIds": [PNG_ID], "corners": [{"x": x, "y": y, "z": z} for x, y, z in corners],
            **({} if reveal is ABSENT else {"tifoReveal": reveal})}]}],
        "assets": [{"id": PNG_ID, "contentType": "image/png", "contentPath": f"assets/{PNG_ID}.png",
                    "byteLength": len(image)}],
    }
    hashes = {entry: hashlib.sha256(data).hexdigest() for entry, data in contents.items()}
    if cloud_hash:
        hashes["point-cloud/field.ply"] = cloud_hash
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as archive:
        archive.writestr("manifest.json", json.dumps({"packageVersion": 2, "match": match, "sha256": hashes}))
        for entry, data in contents.items():
            archive.writestr(entry, data)
    return buffer.getvalue()


class TifoTests(unittest.TestCase):
    def describe(self, data):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "match.zip"
            path.write_bytes(data)
            return fusion_description.describe_scene(path, "/srv/match.zip")

    def test_a_tifo_becomes_a_turned_flag_on_its_corners_plane_revealed_on_load(self):
        scene = self.describe(tifo_package())
        sha = hashlib.sha256(png(960, 360)).hexdigest()
        self.assertEqual(scene["assets"], [{"id": f"FLAG-{sha[:16]}", "kind": "flag", "sha256": sha,
                                            "source": {"package": "/srv/match.zip", "entry": f"assets/{PNG_ID}.png"}}])
        [instance] = scene["instances"]
        self.assertEqual(instance["id"], "flag-tifo-PLC-tifo")
        flag = instance["flag"]
        self.assertAlmostEqual(flag["width_m"], 20)
        self.assertAlmostEqual(flag["height_m"], 4 * math.sqrt(2))
        self.assertEqual(flag["artwork_rotation"], 90)
        self.assertEqual({key: flag[key] for key in ("amplitude_m", "frequency", "speed", "material")},
                         {"amplitude_m": 1.1, "frequency": 1.35, "speed": 5.5, "material": "cloth"})
        self.assertEqual(flag["reveal"], {"direction": "center_horizontal", "duration_s": 2, "start": "load"})
        # u along -X, v down the rake, u x v away from a viewer above the stand; the
        # top-left corner moved towards the viewer by the wave's peak, 1.53 x 1.1 m.
        h, offset = math.sqrt(0.5), 1.53 * 1.1
        expected = [[-1, 0, 0, 10], [0, h, h, 40 - offset * h], [0, h, -h, -6 + offset * h], [0, 0, 0, 1]]
        for row, want in zip(instance["model"], expected):
            for value, target in zip(row, want):
                self.assertAlmostEqual(value, target)

    def test_every_fusion_reveal_direction_keeps_its_direction_and_duration(self):
        model = self.describe(tifo_package())["instances"][0]["model"]
        for fusion, chr_direction, duration in [("left-to-right", "left_to_right", 2),
                                                ("right-to-left", "right_to_left", 0.25),
                                                ("top-to-bottom", "top_to_bottom", 90),
                                                ("bottom-to-top", "bottom_to_top", 3600),
                                                ("center-horizontal", "center_horizontal", 2.5),
                                                ("center-vertical", "center_vertical", 7)]:
            with self.subTest(fusion):
                [instance] = self.describe(tifo_package(reveal={"direction": fusion, "duration": duration}))["instances"]
                self.assertEqual(instance["flag"]["reveal"],
                                 {"direction": chr_direction, "duration_s": duration, "start": "load"})
                # The reveal leaves the raked rectangle and the artwork turn as they are.
                self.assertEqual(instance["model"], model)
                self.assertEqual(instance["flag"]["artwork_rotation"], 90)

    def test_a_tifo_without_a_reveal_is_static_and_a_reveal_lasts_2_s_by_default(self):
        for case, reveal in [("absent", ABSENT), ("null", None)]:
            with self.subTest(case):
                [instance] = self.describe(tifo_package(reveal=reveal))["instances"]
                self.assertNotIn("reveal", instance["flag"])
        for reveal in [{"direction": "center-vertical"}, {"direction": "center-vertical", "duration": None}]:
            [instance] = self.describe(tifo_package(reveal=reveal))["instances"]
            self.assertEqual(instance["flag"]["reveal"], {"direction": "center_vertical", "duration_s": 2, "start": "load"})

    def test_unknown_reveal_directions_and_invalid_durations_name_the_placement(self):
        for case, reveal, message in [
                ("unknown direction", {"direction": "diagonal", "duration": 2}, "direction \"diagonal\""),
                ("chr spelling", {"direction": "left_to_right", "duration": 2}, "direction \"left_to_right\""),
                ("no direction", {"duration": 2}, "direction null"),
                ("zero duration", {"direction": "left-to-right", "duration": 0}, "duration 0"),
                ("negative duration", {"direction": "top-to-bottom", "duration": -1}, "duration -1"),
                ("text duration", {"direction": "top-to-bottom", "duration": "2"}, "duration \"2\""),
                ("not an object", "left-to-right", "must be an object")]:
            with self.subTest(case), self.assertRaises(fusion_description.ConversionError) as raised:
                self.describe(tifo_package(reveal=reveal))
            self.assertIn("Tifo PLC-tifo", str(raised.exception))
            self.assertIn(message, str(raised.exception))

    def test_tifos_off_their_rectangle_translucent_or_with_a_bad_cloud_are_refused(self):
        skewed = [CORNERS[0], CORNERS[1], (-9, 44, -2), CORNERS[3]]
        for case, data in [("skewed corners", tifo_package(corners=skewed)),
                           ("half opacity", tifo_package(opacity=0.5)),
                           ("cloud checksum", tifo_package(cloud_hash="0" * 64))]:
            with self.subTest(case), self.assertRaises(fusion_description.ConversionError):
                self.describe(data)


class DescribeSceneTests(unittest.TestCase):
    def describe(self, model, pose=None, opacity=None, **options):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "match.zip"
            path.write_bytes(package(model, png(200, 100), pose, opacity))
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

    def test_image_opacity_is_sent_as_it_is_and_invalid_values_are_refused(self):
        model = glb([{"mesh": 0}], {"componentType": 5126, "min": [-1, 0, -1], "max": [1, 1, 1]})
        scene = self.describe(model)
        self.assertNotIn("opacity", scene["instances"][0])
        self.assertEqual(scene["instances"][1]["opacity"], 1)
        for opacity in (0, 0.35, 1):
            self.assertEqual(self.describe(model, opacity=opacity)["instances"][1]["opacity"], opacity)
        for opacity in (35, -0.1, "0.35", True):
            with self.assertRaisesRegex(fusion_description.ConversionError, "PLC-image: assetOpacity"):
                self.describe(model, opacity=opacity)

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
