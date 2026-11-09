"""Independently verify encoded output; this CPU decoder is not in the server."""

import argparse
import json
from fractions import Fraction
from pathlib import Path

import av
import numpy as np
from PIL import Image


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--scene", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--ledger", type=Path, required=True)
    parser.add_argument("--bypass", type=Path, required=True)
    parser.add_argument("--reference", type=Path)
    args = parser.parse_args()
    scene = json.loads(args.scene.read_text())
    ledger = [json.loads(line) for line in args.ledger.read_text().splitlines()]
    for sequence, row in enumerate(ledger):
        ordinal = sequence % len(scene["frames"])
        expected = scene["frames"][ordinal]
        assert row["sequence"] == sequence
        assert row["source_ordinal"] == ordinal
        assert row["source_pts"] == expected["clip_pts"]
        assert row["present_index"] == expected["present_index"]
        assert row["ti_row_index"] == expected["ti_row_index"]
    with av.open(str(args.bypass)) as container:
        bypass = {
            index: frame.to_ndarray(format="rgb24")
            for index, frame in enumerate(container.decode(video=0))
        }
    original_prefix = []
    with av.open(scene["video"]) as container:
        for frame in container.decode(video=0):
            original_prefix.append(frame.to_ndarray(format="rgb24"))
            if len(original_prefix) == 9:
                break
    changed = False
    identical = 0
    count = 0
    reference_errors = {}
    with av.open(str(args.output)) as container:
        first_time = None
        for index, frame in enumerate(container.decode(video=0)):
            timestamp = frame.pts * frame.time_base
            if first_time is None:
                first_time = timestamp
            assert timestamp - first_time == Fraction(index, 30)
            assert (frame.width, frame.height) == (scene["width"], scene["height"])
            reference = args.reference / f"frame-{index:06d}.png" if args.reference else None
            if index in bypass or (reference and reference.is_file()):
                pixels = frame.to_ndarray(format="rgb24")
                if index < len(original_prefix):
                    error = np.abs(
                        pixels.astype(np.int16) - original_prefix[index].astype(np.int16)
                    ).mean()
                    assert error < 5, f"Original video was corrupted at frame {index}: {error}"
                if index in bypass:
                    same = np.array_equal(pixels, bypass[index])
                    if scene["frames"][index]["ti_row_index"] is None:
                        assert same, f"Artwork altered uncovered frame {index}"
                        identical += 1
                    else:
                        changed |= not same
                if reference and reference.is_file():
                    with Image.open(reference) as image:
                        expected = np.asarray(image.convert("RGB"), dtype=np.int16)
                    reference_errors[index] = float(
                        np.abs(pixels.astype(np.int16) - expected).mean()
                    )
                    assert reference_errors[index] < 5, (index, reference_errors[index])
            count += 1
    assert count == len(ledger), (count, len(ledger))
    assert identical >= 9 and changed
    if count > len(scene["frames"]):
        assert ledger[len(scene["frames"])]["timecode"] == "00:00:49:12"
    print(
        json.dumps(
            {
                "frames": count,
                "pts_mismatches": 0,
                "loops": count // len(scene["frames"]),
                "initial_no_ti_frames_identical_to_bypass": identical,
                "artwork_changes_visible": changed,
                "reference_rgb_mae_codes_after_lossy_encoding": reference_errors,
                "verification_decoder": "Separate CPU decoder, not part of the production pipeline",
            }
        )
    )


if __name__ == "__main__":
    main()
