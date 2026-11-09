"""Verify encoded output in a separate CPU decoder; never reads production GPU memory."""

import argparse
import json
import subprocess
from fractions import Fraction
from pathlib import Path


def run(*args):
    return subprocess.check_output(args, text=True)


def decoded_hashes(path, frames):
    output = run("ffmpeg", "-v", "error", "-i", str(path), "-frames:v", str(frames), "-f", "framemd5", "-")
    return [line.rsplit(",", 1)[-1].strip() for line in output.splitlines() if not line.startswith("#")]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--scene", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--ledger", type=Path, required=True)
    parser.add_argument("--bypass", type=Path)
    args = parser.parse_args()
    scene = json.loads(args.scene.read_text())
    ledger = [json.loads(line) for line in args.ledger.read_text().splitlines()]
    assert ledger, "No processed frames"
    for sequence, item in enumerate(ledger):
        ordinal = sequence % len(scene["frames"])
        frame = scene["frames"][ordinal]
        assert item["sequence"] == item["output_pts"] == sequence
        assert item["source_loop"] == sequence // len(scene["frames"])
        assert item["clip_ordinal"] == ordinal
        assert item["present_index"] == frame["present_index"]
        assert item["ti_row_index"] == frame["ti_row_index"]
        assert item["original_source_pts"] == frame["source_pts"]
        assert item["source_pts"] * Fraction(*item["source_time_base"]) == frame["clip_pts"] * Fraction(
            frame["time_base_num"], frame["time_base_den"]
        )
        wrapped = sequence % (24 * 3600 * 30)
        tc = f"{wrapped // 108000:02}:{wrapped // 1800 % 60:02}:{wrapped // 30 % 60:02}:{wrapped % 30:02}"
        assert item["timecode"] == tc
    probe = json.loads(
        run("ffprobe", "-v", "error", "-select_streams", "v:0", "-show_frames", "-show_streams", "-of", "json", str(args.output))
    )
    stream = probe["streams"][0]
    frames = probe["frames"]
    assert stream["codec_name"] == "h264"
    assert (stream["width"], stream["height"]) == (scene["width"], scene["height"])
    assert len(frames) == len(ledger), "Encoded frame count differs from identity ledger"
    first_pts = frames[0]["best_effort_timestamp"]
    for i, frame in enumerate(frames):
        assert (frame["best_effort_timestamp"] - first_pts) * Fraction(stream["time_base"]) == Fraction(i, 30)
    report = {"frames": len(frames), "pts_mismatches": 0, "loops": ledger[-1]["source_loop"], "decoded": "independent CPU verifier"}
    if args.bypass:
        composed = decoded_hashes(args.output, 60)
        bypass = decoded_hashes(args.bypass, 60)
        boundary = next(i for i, frame in enumerate(scene["frames"]) if frame["ti_row_index"] is not None)
        assert composed[:boundary] == bypass[:boundary], "Artwork changed a no-Ti initial frame"
        assert any(a != b for a, b in zip(composed[boundary:], bypass[boundary:], strict=True)), "Shader did not alter output"
        report["initial_no_ti_frames_identical"] = boundary
        report["scene_changes_visible"] = True
    print(json.dumps(report, sort_keys=True))


if __name__ == "__main__":
    main()
