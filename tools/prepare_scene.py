"""Prepare private, backend-neutral scene data without decoding video frames."""

import argparse
import hashlib
import json
import os
from fractions import Fraction
from pathlib import Path

import cv2
import numpy as np
import pyarrow.parquet as pq
import tomllib
from PIL import Image


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def unit_to_world(xyz):
    points = np.asarray(xyz, dtype=np.float64)
    if points.shape != (4, 3) or not np.isfinite(points).all() or np.any(points[:, 2]):
        raise ValueError("Expected four finite ground-plane XYZ corners.")
    if not cv2.isContourConvex(points[:, :2].astype(np.float32)):
        raise ValueError("Texture quads must be convex and nondegenerate.")
    equations, values = [], []
    for (u, v), (x, y, _) in zip([(0, 0), (1, 0), (1, 1), (0, 1)], points, strict=True):
        equations.extend([[u, v, 1, 0, 0, 0, -x * u, -x * v], [0, 0, 0, u, v, 1, -y * u, -y * v]])
        values.extend([x, y])
    return np.append(np.linalg.solve(equations, values), 1).reshape(3, 3)


def frame_quad(ti, world, texture, width, height):
    if ti is None:
        return {"valid": False, "texture": texture}
    scale = np.sqrt(width * height)
    chart = np.array([[scale / 2, 0, width / 2], [0, scale / 2, height / 2], [0, 0, 1]])
    transform = chart @ np.asarray(ti, dtype=np.float64) @ world
    corners = transform @ np.array([[0, 1, 1, 0], [0, 0, 1, 1], [1, 1, 1, 1]])
    denominator = corners[2]
    if (
        not np.isfinite(corners).all()
        or np.any(np.abs(denominator) < 1e-12 * np.max(np.abs(corners), axis=0))
        or np.any(np.sign(denominator) != np.sign(denominator[0]))
    ):
        return {"valid": False, "texture": texture}
    pixels = corners[:2] / denominator
    low = np.maximum(0, np.floor(pixels.min(axis=1) - 2)).astype(int)
    high = np.minimum([width, height], np.ceil(pixels.max(axis=1) + 3)).astype(int)
    return {
        "valid": bool(np.all(high > low)),
        "texture": texture,
        "inverse": np.linalg.inv(transform).ravel().tolist(),
        "bounds": [int(low[0]), int(low[1]), int(high[0]), int(high[1])],
    }


def prepare(config_path, destination):
    config = tomllib.loads(config_path.read_text())
    bundle = Path(config["bundle"]).resolve()
    run = (bundle / config["reference_run"]).resolve()
    drawing_path = (bundle / config["quads"]).resolve()
    drawing = json.loads(drawing_path.read_text())
    declaration = json.loads((run / "declaration.json").read_text())
    video = (run / declaration["video"]).resolve()
    initialization = json.loads((run / "init/receipt.json").read_text())
    if digest(video) != initialization["source_sha256"]:
        raise ValueError("The mask cache belongs to another video.")
    ti_path = run / "inputs/Ti.parquet"
    if digest(ti_path) != drawing["ti_sha256"]:
        raise ValueError("The drawing and camera transforms have different identities.")
    rows = pq.read_table(run / "inputs/frame-map.parquet").to_pylist()
    poses = pq.read_table(ti_path).to_pylist()
    width, height = rows[0]["native_width"], rows[0]["native_height"]
    times = [
        row["clip_pts"] * Fraction(row["clip_time_base_num"], row["clip_time_base_den"])
        for row in rows
    ]
    step = times[1] - times[0]
    if step <= 0 or any(time != i * step for i, time in enumerate(times)):
        raise ValueError("This simulator requires a zero-based constant-frame-rate clip.")
    rate = 1 / step
    if rate != 30:
        raise ValueError("The first simulation supports 30 fps non-drop-frame timecode.")
    if len(rows) != declaration["clip_frames"] or drawing["case"] != declaration["case"]:
        raise ValueError("The scene identity or frame coverage is inconsistent.")
    destination.mkdir(parents=True, mode=0o700, exist_ok=False)
    destination = destination.resolve()
    textures, texture_indices, prepared = [], {}, []
    quads = {quad["name"]: quad for quad in drawing["quads"]}
    for name, (filename, rotation) in declaration["assignments_by_name"].items():
        if Path(filename).name != filename or rotation not in (0, 90, 180, 270):
            raise ValueError("Invalid artwork assignment.")
        key = (filename, rotation)
        if key not in texture_indices:
            index = len(textures)
            texture_indices[key] = index
            with Image.open(run / "inputs" / filename) as image:
                pixels = np.asarray(image.convert("RGBA"))
            pixels = np.rot90(pixels, -rotation // 90).astype(np.float32) / 255
            pixels[:, :, :3] *= pixels[:, :, 3:]
            path = destination / f"logo-{index}.rgba32f"
            levels = []
            with path.open("xb") as stream:
                while True:
                    h, w = pixels.shape[:2]
                    levels.append({"width": w, "height": h, "offset": stream.tell()})
                    stream.write(pixels.astype("<f4").tobytes())
                    if (w, h) == (1, 1):
                        break
                    pixels = cv2.resize(
                        pixels, (max(1, w // 2), max(1, h // 2)), interpolation=cv2.INTER_AREA
                    )
            textures.append({"path": str(path), "levels": levels, "format": "rgba32float"})
        prepared.append((unit_to_world(quads[name]["corners_world_metres"]), texture_indices[key]))
    frames = []
    for ordinal, row in enumerate(rows):
        if row["clip_ordinal"] != ordinal or row["media_id"] != drawing["source_media_id"]:
            raise ValueError("Invalid frame identity.")
        ti_index = row["ti_row_index"]
        ti = None
        if ti_index is not None:
            pose = poses[ti_index]
            if (pose["present_index"], pose["pts"]) != (row["present_index"], row["source_pts"]):
                raise ValueError("Ti does not match the source frame.")
            ti = pose["Ti"]
        frames.append(
            {
                "clip_pts": row["clip_pts"],
                "time_base_num": row["clip_time_base_num"],
                "time_base_den": row["clip_time_base_den"],
                "present_index": row["present_index"],
                "source_pts": row["source_pts"],
                "ti_row_index": ti_index,
                "quads": [
                    frame_quad(ti, world, texture, width, height) for world, texture in prepared
                ],
            }
        )
    masks = destination / "masks.r8"
    with masks.open("xb") as stream:
        for ordinal in range(len(rows)):
            with Image.open(run / "render/scene-masks" / f"{ordinal:06d}.png") as image:
                if image.mode != "L" or image.size != (width, height):
                    raise ValueError(f"Unexpected mask format at frame {ordinal}.")
                stream.write(image.tobytes())
    scene = {
        "version": 1,
        "case": drawing["case"],
        "video": str(video),
        "width": width,
        "height": height,
        "fps_num": rate.numerator,
        "fps_den": rate.denominator,
        "opacity": declaration["global_logo_opacity"],
        "textures": textures,
        "mask_path": str(masks),
        "frames": frames,
        "pins": {
            "video_sha256": initialization["source_sha256"],
            "drawing_sha256": digest(drawing_path),
            "ti_sha256": drawing["ti_sha256"],
        },
        "policy": {
            "timecode": "30 fps NDF; continuous across source loops",
            "color": "Encoded RGB composition; preserve input color metadata separately",
            "missing_ti": "No artwork; never invent or interpolate transforms",
            "sampling": "Premultiplied mipmaps; inverse-Jacobian LOD; 2x2 resolve",
        },
    }
    manifest = destination / "scene.json"
    manifest.write_text(json.dumps(scene, allow_nan=False, separators=(",", ":")) + "\n")
    print(f"Prepared {len(frames)} frames and {len(textures)} textures: {manifest}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    os.umask(0o077)
    cv2.setNumThreads(2)
    prepare(args.config, args.output)


if __name__ == "__main__":
    main()
