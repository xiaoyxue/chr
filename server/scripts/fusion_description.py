#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Converts a Fusion offline ZIP into a chr-render-description-v1 scene.

It follows the pitch-studio client's conversion of a match's Fusion placements
(client/pitch-studio/src/render/render-description.ts, describeScene with no local
placements), so the result equals the client's 导出渲染描述 for the same package: GLB
bounds as three.js reports them (Box3.setFromObject over GLTFLoader's accessor
bounds), images by their pixel size, and assets read by chr-server from the ZIP.
A package v2 Tifo (#210) becomes the flag the studio seeds from it, unedited and
with its cloud taken as confirmed: on its corners' plane, with the studio's flag
defaults, its artwork turned by assetRotation and revealed when the scene loads.

Without --camera it writes `{"scene": …}`, the shape `description.load` takes; with
--camera it writes a complete description for `chr-server --description` or
`chr-renderer-cli render --description`, using the `camera` of the given file.
--move, --scale and --rotate edit a placement before converting, as the studio's
inspector does: they move it in pitch metres; scale a 3D model about its bottom centre
(`scaleX`) or a 2D area about its centre; and turn it by `rz` degrees, a model about its
own up axis without refitting it, an area about its centre.

    uv run server/scripts/fusion_description.py MATCH.zip --camera start.json \\
        --move PLC-… 0 10 --scale PLC-… 0.5 --rotate PLC-… 30 --output scene.json
"""

import argparse
import hashlib
import json
import math
import re
import struct
import sys
import zipfile
from pathlib import Path

ENTRY = re.compile(r"^(?:manifest\.json|assets/[A-Za-z0-9][A-Za-z0-9_-]{0,63}\.(?:png|jpg|webp|glb)"
                   r"|point-cloud/[A-Za-z0-9][A-Za-z0-9_.-]{0,63}\.ply)$")
EXTENSIONS = {"image/png": ".png", "image/jpeg": ".jpg", "image/webp": ".webp", "model/gltf-binary": ".glb"}
MAX_INSTANCES = 63
# The studio's flag defaults (client/pitch-studio/src/placements/flag.ts) for a seeded tifo.
TIFO_FLAG = {"amplitude_m": 0.32, "frequency": 1.35, "speed": 1, "material": "simple"}
TIFO_OFFSET = -0.5
V2_COORDINATES = {"z": "down", "swingSpace": "field", "motionSpace": "field",
                  "motionOrigin": "neutral-model-bottom-center", "motionScale": "neutral-pose-multiplier"}
# GLTFLoader's getNormalizedComponentScale, by accessor componentType.
NORMALIZED_SCALE = {5120: 1 / 127, 5121: 1 / 255, 5122: 1 / 32767, 5123: 1 / 65535}


class ConversionError(Exception):
    pass


def require(condition, message):
    if not condition:
        raise ConversionError(message)


def number(value):
    require(isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value),
            "Invalid Fusion number.")
    return value


def read_package(path):
    """The manifest's match and the bytes of each asset, verified as parseOfflinePackage does."""
    with zipfile.ZipFile(path) as archive:
        names = archive.namelist()
        require(len(names) == len(set(names)) and all(ENTRY.match(name) for name in names),
                "The offline ZIP contains duplicate or unexpected files.")
        files = {name: archive.read(name) for name in names}
    require("manifest.json" in files, "The offline package is missing manifest.json.")
    manifest = json.loads(files["manifest.json"].decode("utf-8"))
    require(manifest.get("packageVersion") in (1, 2), "Unsupported offline package version.")
    match, hashes = manifest["match"], manifest["sha256"]
    cloud = (match.get("field") or {}).get("pointCloud")
    if cloud is not None:
        entry = cloud.get("contentPath")
        data = files.get(entry)
        require(isinstance(entry, str) and entry.startswith("point-cloud/") and data is not None
                and cloud.get("byteLength") == len(data), "The offline point cloud is missing or has invalid metadata.")
        require(hashes.get(entry) == hashlib.sha256(data).hexdigest(), "Point cloud checksum failed.")
    assets = {}
    for asset in match["assets"]:
        entry = f"assets/{asset['id']}{EXTENSIONS.get(asset['contentType'], '')}"
        data = files.get(entry)
        require(asset.get("contentPath") == entry and data is not None and asset.get("byteLength") == len(data),
                f"Offline asset {asset['id']} is missing or has invalid metadata.")
        digest = hashlib.sha256(data).hexdigest()
        require(hashes.get(entry) == digest, f"Material checksum failed: {asset['id']}.")
        assets[asset["id"]] = {"contentType": asset["contentType"], "entry": entry, "sha256": digest, "data": data}
    clouds = int(cloud is not None)
    require(len(files) == len(assets) + 1 + clouds and len(hashes) == len(assets) + clouds,
            "The package contains missing or unreferenced materials.")
    return match, assets


def placements(match, assets):
    """The match's placements as mapFusionMatch builds them (z, rx, ry and rz are 0)."""
    coordinates = match["coordinates"]
    version = match.get("schemaVersion")
    require(version in (1, 2) and match.get("sport") == "football"
            and coordinates.get("unit") == "meters" and coordinates.get("origin") == "field-center"
            and coordinates.get("x") == "right" and coordinates.get("y") == "down-in-top-view"
            and coordinates.get("modelUp") == "gltf-y-up"
            and (version == 1 or all(coordinates.get(k) == v for k, v in V2_COORDINATES.items())),
            "Unsupported Fusion sport or coordinate convention.")
    result = []
    for ad in match["ads"]:
        require(ad["type"] in ("2D", "3D", "Tifo"), "Unsupported Fusion Ad type.")
        if ad["type"] == "Tifo":
            require(match["field"].get("pointCloud") is not None, "A Tifo needs the package's point cloud.")
            result.extend(tifo(source, assets) for source in ad["placements"])
            continue
        for source in ad["placements"]:
            require(source.get("coordinateSpace") == "pitch", f"Placement {source['id']} uses a legacy canvas.")
            asset_ids = source.get("assetIds", [])
            require(len(asset_ids) <= 1, f"Placement {source['id']} has multiple legacy assets.")
            asset = assets.get(asset_ids[0]) if asset_ids else None
            require(not asset_ids or asset is not None, f"Assigned asset for {source['id']} is missing.")
            require(asset is None or (asset["contentType"] == "model/gltf-binary") == (ad["type"] == "3D"),
                    "Fusion placement material does not match its Ad type.")
            width, height = number(source["width"]), number(source["height"])
            require(width > 0 and height > 0, "Invalid Fusion placement dimensions.")
            slot = {"id": source["id"], "type": ad["type"], "asset": asset_ids[0] if asset_ids else None,
                    "x": number(source["x"]), "y": number(source["y"]), "z": 0, "rz": 0}
            if ad["type"] == "2D":
                rotation = number(source["assetRotation"])
                require(rotation in (0, 90, 180, 270), "Invalid image rotation.")
                # #188: absent in older manifests, where the image is drawn as it is.
                opacity = source.get("assetOpacity", 1)
                require(isinstance(opacity, (int, float)) and not isinstance(opacity, bool) and 0 <= opacity <= 1,
                        f"Placement {source['id']}: assetOpacity must be a number from 0 to 1 (35% is 0.35), "
                        f"not {json.dumps(opacity)}.")
                slot.update(sizeX=width, sizeY=height, assetRotation=rotation, assetOpacity=opacity)
            else:
                pose = {key: number(source["modelPose"][key]) for key in ("x", "y", "scale", "yaw")}
                require(abs(pose["x"]) <= 1 and abs(pose["y"]) <= 1 and 0.01 <= pose["scale"] <= 1
                        and 0 <= pose["yaw"] < 360, "Invalid Fusion model pose.")
                animation = ("swingAxis", "swingAmplitude", "swingPeriod", "rotationMode", "motion")
                pose.update({key: source["modelPose"][key] for key in animation if key in source["modelPose"]})
                slot.update(region={"width": width, "height": height}, modelPose=pose, scaleX=1)
            result.append(slot)
    return result


def tifo(source, assets):
    """A Tifo placement: its image, its corners' rectangle and how it appears."""
    name = source["id"]
    require(source.get("coordinateSpace") == "point-cloud", f"Tifo {name} must be placed on the point cloud.")
    asset_ids = source.get("assetIds", [])
    require(len(asset_ids) == 1 and asset_ids[0] in assets
            and assets[asset_ids[0]]["contentType"] in ("image/png", "image/jpeg"),
            f"Tifo {name} needs one PNG or JPEG image.")
    corners = [[number(c[k]) for k in ("x", "y", "z")] for c in source.get("corners", [])]
    require(len(corners) == 4, f"Tifo {name} needs four corners.")
    c0, c1, c2, c3 = corners
    u, v = [b - a for a, b in zip(c0, c1)], [b - a for a, b in zip(c0, c3)]
    width, height = math.hypot(*u), math.hypot(*v)
    require(width > 0 and height > 0
            and all(abs(a + c - b - d) < 1e-3 for a, b, c, d in zip(c0, c1, c2, c3))
            and abs(sum(a * b for a, b in zip(u, v))) < 1e-4 * width * height
            and abs(width - number(source["width"])) < 1e-3 and abs(height - number(source["height"])) < 1e-3,
            f"Tifo {name}: its corners are not the {source['width']} x {source['height']} m rectangle.")
    rotation = number(source.get("assetRotation", 0))
    require(rotation in (0, 90, 180, 270), "Invalid image rotation.")
    require(source.get("assetOpacity", 1) == 1, f"Tifo {name}: chr flags are opaque; assetOpacity must be 1.")
    reveal = source.get("tifoReveal")
    if reveal is not None:
        require(reveal.get("direction") == "center-horizontal" and 0 < number(reveal.get("duration")) <= 60,
                f"Tifo {name}: unsupported tifoReveal.")
    return {"id": name, "type": "Tifo", "asset": asset_ids[0], "origin": c0,
            "u": [x / width for x in u], "v": [x / height for x in v], "width": width, "height": height,
            "assetRotation": int(rotation), "reveal": reveal and reveal["duration"]}


def flag_model(slot, offset):
    """flagModel: columns u, v, their cross product and the top-left corner moved along it."""
    u, v = slot["u"], slot["v"]
    n = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]]
    o = [a + b * offset for a, b in zip(slot["origin"], n)]
    return [[u[i], v[i], n[i], o[i]] for i in range(3)] + [[0, 0, 0, 1]]


def image_size(data, content_type):
    """Pixel width and height of a PNG or JPEG, the image kinds chr-server imports."""
    if content_type == "image/png":
        require(data[:8] == b"\x89PNG\r\n\x1a\n" and data[12:16] == b"IHDR", "Invalid PNG material.")
        return struct.unpack(">II", data[16:24])
    require(content_type == "image/jpeg", f"chr-server does not import {content_type} images.")
    offset = 2
    while offset + 9 < len(data):
        require(data[offset] == 0xFF, "Invalid JPEG material.")
        marker = data[offset + 1]
        if marker in (0xC0, 0xC1, 0xC2, 0xC3, 0xC5, 0xC6, 0xC7, 0xC9, 0xCA, 0xCB, 0xCD, 0xCE, 0xCF):
            height, width = struct.unpack(">HH", data[offset + 5:offset + 9])
            return width, height
        offset += 2 + struct.unpack(">H", data[offset + 2:offset + 4])[0]
    raise ConversionError("JPEG material has no frame header.")


def glb_json(data):
    require(data[:4] == b"glTF" and data[16:20] == b"JSON", "Invalid embedded GLB material.")
    (length,) = struct.unpack("<I", data[12:16])
    return json.loads(data[20:20 + length])


def blended_materials(gltf):
    """Names of the GLB's materials with alphaMode BLEND (blendedMaterials)."""
    return [material.get("name", "unnamed") for material in gltf.get("materials", [])
            if material.get("alphaMode") == "BLEND"]


def matmul(a, b):
    return [[sum(a[i][k] * b[k][j] for k in range(4)) for j in range(4)] for i in range(4)]


def node_matrix(node):
    """A glTF node's local transform, row-major."""
    if "matrix" in node:
        m = node["matrix"]
        return [[m[j * 4 + i] for j in range(4)] for i in range(4)]
    x, y, z, w = node.get("rotation", [0, 0, 0, 1])
    sx, sy, sz = node.get("scale", [1, 1, 1])
    tx, ty, tz = node.get("translation", [0, 0, 0])
    return [
        [(1 - 2 * (y * y + z * z)) * sx, 2 * (x * y - z * w) * sy, 2 * (x * z + y * w) * sz, tx],
        [2 * (x * y + z * w) * sx, (1 - 2 * (x * x + z * z)) * sy, 2 * (y * z - x * w) * sz, ty],
        [2 * (x * z - y * w) * sx, 2 * (y * z + x * w) * sy, (1 - 2 * (x * x + y * y)) * sz, tz],
        [0, 0, 0, 1],
    ]


def primitive_box(gltf, primitive):
    """GLTFLoader's computeBounds: the POSITION accessor's bounds, widened by morph targets."""
    accessors = gltf["accessors"]
    position = primitive["attributes"].get("POSITION")
    if position is None:
        return None
    accessor = accessors[position]
    require("min" in accessor and "max" in accessor, "A GLB POSITION accessor has no min/max.")
    scale = NORMALIZED_SCALE[accessor["componentType"]] if accessor.get("normalized") else 1
    low, high = [v * scale for v in accessor["min"][:3]], [v * scale for v in accessor["max"][:3]]
    displacement = [0.0, 0.0, 0.0]
    for target in primitive.get("targets", []):
        if "POSITION" not in target:
            continue
        morph = accessors[target["POSITION"]]
        if "min" not in morph or "max" not in morph:
            continue
        morph_scale = NORMALIZED_SCALE[morph["componentType"]] if morph.get("normalized") else 1
        displacement = [max(d, max(abs(a), abs(b)) * morph_scale)
                        for d, a, b in zip(displacement, morph["min"], morph["max"])]
    return [a - d for a, d in zip(low, displacement)], [b + d for b, d in zip(high, displacement)]


def glb_bounds(gltf):
    """Box3.setFromObject(gltf.scene): each primitive's box transformed by its node's world matrix."""
    low, high = [math.inf] * 3, [-math.inf] * 3
    nodes = gltf.get("nodes", [])
    pending = [(index, [[float(i == j) for j in range(4)] for i in range(4)])
               for index in gltf["scenes"][gltf.get("scene", 0)]["nodes"]]
    while pending:
        index, parent = pending.pop()
        node = nodes[index]
        require("skin" not in node and "EXT_mesh_gpu_instancing" not in node.get("extensions", {}),
                "Skinned or instanced GLB meshes are not supported.")
        world = matmul(parent, node_matrix(node))
        if "mesh" in node:
            for primitive in gltf["meshes"][node["mesh"]]["primitives"]:
                box = primitive_box(gltf, primitive)
                if box is None:
                    continue
                for corner in ((x, y, z) for x in (box[0][0], box[1][0])
                               for y in (box[0][1], box[1][1]) for z in (box[0][2], box[1][2])):
                    w = sum(world[3][k] * corner[k] for k in range(3)) + world[3][3]
                    for i in range(3):
                        value = (sum(world[i][k] * corner[k] for k in range(3)) + world[i][3]) / w
                        low[i], high[i] = min(low[i], value), max(high[i], value)
        pending.extend((child, world) for child in node.get("children", []))
    require(all(map(math.isfinite, low + high)) and (high[0] - low[0]) + (high[2] - low[2]) > 0,
            "The model has no finite ground footprint.")
    return {"min": low, "max": high}


def fit_fusion_model(width, depth, slot):
    """fitFusionModel: Fusion's heading and size fit the region; scaleX and rz act after it."""
    region, pose = slot["region"], slot["modelPose"]
    heading = pose["yaw"] * math.pi / 180
    cosine, sine = abs(math.cos(heading)), abs(math.sin(heading))
    rotated_width = width * cosine + depth * sine
    rotated_depth = width * sine + depth * cosine
    neutral = min(region["width"] / max(rotated_width, 1e-8),
                  region["height"] / max(rotated_depth, 1e-8)) * pose["scale"]
    return {"scale": neutral * slot["scaleX"], "yaw": heading + slot["rz"] * math.pi / 180,
            "x": slot["x"] + pose["x"] * max(0, region["width"] - rotated_width * neutral) / 2,
            "y": slot["y"] + pose["y"] * max(0, region["height"] - rotated_depth * neutral) / 2}


def glb_model(slot, bounds):
    """glbModel: centre the glTF scene on its footprint, stand it on the ground and fit it."""
    low, high = bounds["min"], bounds["max"]
    fit = fit_fusion_model(high[0] - low[0], high[2] - low[2], slot)
    c, s = math.cos(fit["yaw"]) * fit["scale"], math.sin(fit["yaw"]) * fit["scale"]
    centre = [-(low[0] + high[0]) / 2, -(low[2] + high[2]) / 2, low[1]]
    return [
        [c, -s, 0, fit["x"] + c * centre[0] - s * centre[1]],
        [s, c, 0, fit["y"] + s * centre[0] + c * centre[1]],
        [0, 0, fit["scale"], slot["z"] - 0.015 + fit["scale"] * centre[2]],
        [0, 0, 0, 1],
    ]


def wire_motion(pose, pivot):
    """wireMotion: the pose's rotation and timed motion about `pivot` (#157), or None."""
    mode = pose.get("rotationMode", "swing")
    amplitude = pose.get("swingAmplitude", 0) * math.pi / 180
    axis = pose.get("swingAxis")
    rotates = axis is not None and (mode == "continuous" or amplitude > 0)
    motion = pose.get("motion")
    moves = bool(motion and motion["enabled"])
    if not (rotates or moves):
        return None
    result = {"pivot": pivot}
    if rotates:
        require(axis in ("x", "y", "z") and mode in ("swing", "continuous")
                and 0.1 <= pose.get("swingPeriod", 0) <= 60, "Invalid Fusion model rotation.")
        result["rotation"] = {"axis": {"x": [1, 0, 0], "y": [0, 1, 0], "z": [0, 0, 1]}[axis], "mode": mode,
                              "amplitude": amplitude, "period": pose["swingPeriod"]}
    if moves:
        playback = {"once": "once", "loop": "loop", "ping-pong": "ping_pong"}.get(motion["playback"])
        require(playback and motion["duration"] > 0, "Invalid Fusion model motion.")
        key = lambda k: {"offset": [k["x"], k["y"], k["z"]], "scale": k["scale"]}
        result["path"] = {"start": key(motion["start"]), "end": key(motion["end"]),
                          "duration": motion["duration"], "playback": playback}
    return result


def quad_model(slot, width, height):
    """quadModel: the unit image quad contain-fitted in the placement, image top towards -Y."""
    rotated = slot["assetRotation"] in (90, 270)
    scale = min(slot["sizeX"] / (height if rotated else width), slot["sizeY"] / (width if rotated else height))
    angle = (slot["rz"] + slot["assetRotation"]) * math.pi / 180
    c, s, w, h = math.cos(angle), math.sin(angle), width * scale, height * scale
    return [[c * w, -s * h, 0, slot["x"]], [s * w, c * h, 0, slot["y"]], [0, 0, 1, -0.02], [0, 0, 0, 1]]


def describe_scene(zip_path, package_path=None, moves=(), hidden=(), scales=(), rotations=()):
    """The description scene of the ZIP's Fusion placements, assets read from `package_path`."""
    match, assets = read_package(zip_path)
    every = placements(match, assets)
    slots = {slot["id"]: slot for slot in every if slot["type"] != "Tifo"}
    tifos = [slot for slot in every if slot["type"] == "Tifo"]
    for placement_id in [edit[0] for edit in [*moves, *scales, *rotations]]:
        require(placement_id in slots, f"Unknown placement {placement_id}.")
    for placement_id in hidden:
        require(placement_id in slots or any(t["id"] == placement_id for t in tifos),
                f"Unknown placement {placement_id}.")
    for placement_id, dx, dy in moves:
        slots[placement_id]["x"] += dx
        slots[placement_id]["y"] += dy
    for placement_id, factor in scales:
        require(factor > 0, f"Scale of {placement_id} must be positive.")
        slot = slots[placement_id]
        if slot["type"] == "3D":
            slot["scaleX"] *= factor
        else:
            slot["sizeX"] *= factor
            slot["sizeY"] *= factor
    for placement_id, degrees in rotations:
        slots[placement_id]["rz"] += degrees
    field = match["field"]
    scene = {"ground": {"pitch_m": [number(field["length"]), number(field["width"])]}, "assets": [], "instances": []}
    for slot in slots.values():
        if slot["asset"] is None:
            continue
        asset = assets[slot["asset"]]
        described = {"id": slot["asset"], "source": {"package": str(package_path or Path(zip_path).resolve()),
                                                     "entry": asset["entry"]}, "sha256": asset["sha256"]}
        if slot["type"] == "3D":
            gltf = glb_json(asset["data"])
            bounds = glb_bounds(gltf)
            model = glb_model(slot, bounds)
            fit = fit_fusion_model(bounds["max"][0] - bounds["min"][0], bounds["max"][2] - bounds["min"][2], slot)
            motion = wire_motion(slot["modelPose"], [fit["x"], fit["y"], slot["z"] - 0.015])
            described["kind"] = "glb"
            if blended_materials(gltf):
                described["blend_as"] = "mask"
        else:
            motion = None
            model = quad_model(slot, *image_size(asset["data"], asset["contentType"]))
            described["kind"] = "image_quad"
        if not any(listed["id"] == described["id"] for listed in scene["assets"]):
            scene["assets"].append({key: described[key] for key in ("id", "kind", "source", "sha256", "blend_as")
                                    if key in described})
        scene["instances"].append({"id": slot["id"], "asset": slot["asset"], "model": model,
                                   "visible": slot["id"] not in hidden, **({"motion": motion} if motion else {}),
                                   **({"opacity": slot["assetOpacity"]} if slot["type"] == "2D" else {})})
    package = str(package_path or Path(zip_path).resolve())
    for slot in tifos:
        asset = assets[slot["asset"]]
        asset_id = f"FLAG-{asset['sha256'][:16]}"
        if not any(listed["id"] == asset_id for listed in scene["assets"]):
            scene["assets"].append({"id": asset_id, "kind": "flag", "source": {"package": package, "entry": asset["entry"]},
                                    "sha256": asset["sha256"]})
        flag = {"width_m": slot["width"], "height_m": slot["height"], **TIFO_FLAG,
                **({"artwork_rotation": slot["assetRotation"]} if slot["assetRotation"] else {}),
                **({"reveal": {"direction": "center_horizontal", "duration_s": slot["reveal"], "start": "load"}}
                   if slot["reveal"] else {})}
        scene["instances"].append({"id": f"flag-tifo-{slot['id']}", "asset": asset_id,
                                   "model": flag_model(slot, TIFO_OFFSET), "visible": slot["id"] not in hidden,
                                   "flag": flag})
    require(len(scene["instances"]) <= MAX_INSTANCES,
            f"chr-server places at most {MAX_INSTANCES} instances; this match has {len(scene['instances'])}.")
    return scene


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("zip", type=Path, help="Fusion offline ZIP")
    parser.add_argument("--camera", type=Path,
                        help="a render description whose camera to use; writes a complete description")
    parser.add_argument("--package-path",
                        help="the ZIP's path as chr-server reads it [default: the ZIP's absolute path]")
    parser.add_argument("--move", nargs=3, action="append", default=[], metavar=("PLACEMENT", "DX", "DY"),
                        help="shift a placement by DX, DY metres (X along the length, Y along the width)")
    parser.add_argument("--scale", nargs=2, action="append", default=[], metavar=("PLACEMENT", "FACTOR"),
                        help="resize a placement: a 3D model's scale or a 2D placement's area")
    parser.add_argument("--rotate", nargs=2, action="append", default=[], metavar=("PLACEMENT", "DEGREES"),
                        help="turn a placement about its up axis (a model) or centre (an area)")
    parser.add_argument("--hide", action="append", default=[], metavar="PLACEMENT",
                        help="send a placement as visible: false")
    parser.add_argument("--output", type=Path, help="output JSON file [default: stdout]")
    args = parser.parse_args(argv)
    try:
        moves = [(placement, float(dx), float(dy)) for placement, dx, dy in args.move]
        scales = [(placement, float(factor)) for placement, factor in args.scale]
        rotations = [(placement, float(degrees)) for placement, degrees in args.rotate]
        scene = describe_scene(args.zip, args.package_path, moves, args.hide, scales, rotations)
    except (ConversionError, ValueError, KeyError, zipfile.BadZipFile) as error:
        sys.exit(f"error: {error}")
    if args.camera:
        camera = json.loads(args.camera.read_text())["camera"]
        document = {"profile": "chr-render-description-v1", "camera": camera, "scene": scene}
    else:
        document = {"scene": scene}
    text = json.dumps(document, indent=2, ensure_ascii=False) + "\n"
    if args.output:
        args.output.write_text(text)
    else:
        sys.stdout.write(text)


if __name__ == "__main__":
    main()
