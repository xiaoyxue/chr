#!/usr/bin/env python3
"""Video-only FFmpeg -> UYVY pipe -> GStreamer appsink input probe, not SDI."""

import argparse
from fractions import Fraction
import hashlib
import json
from pathlib import Path
import signal
import subprocess
import sys
import time

import gi

gi.require_version("Gst", "1.0")
gi.require_version("GstApp", "1.0")
from gi.repository import Gst, GstApp


def probe(source):
    result = subprocess.run(
        [
            "ffprobe", "-v", "error", "-select_streams", "v:0",
            "-show_entries",
            "stream=width,height,avg_frame_rate,r_frame_rate,field_order,"
            "sample_aspect_ratio,color_range,color_space,color_transfer,color_primaries",
            "-of", "json", str(source),
        ],
        capture_output=True, text=True, check=True, timeout=30,
    )
    streams = json.loads(result.stdout)["streams"]
    if not streams:
        raise ValueError("input has no video stream")
    stream = streams[0]
    width, height = stream["width"], stream["height"]
    try:
        rate = Fraction(stream["avg_frame_rate"])
        nominal_rate = Fraction(stream["r_frame_rate"])
    except (ValueError, ZeroDivisionError) as error:
        raise ValueError("input must declare a valid frame rate") from error
    if width <= 0 or height <= 0 or width % 2:
        raise ValueError("input must have positive dimensions and an even width")
    if rate <= 0 or rate != nominal_rate:
        raise ValueError("input must declare a positive constant frame rate")
    if stream.get("field_order") != "progressive":
        raise ValueError("only explicitly progressive video is supported")
    if stream.get("sample_aspect_ratio") != "1:1":
        raise ValueError("only square-pixel video is supported")
    if stream.get("color_range") != "tv" or any(
        stream.get(key) != "bt709"
        for key in ("color_space", "color_transfer", "color_primaries")
    ):
        raise ValueError("input must declare limited-range BT.709 SDR colour")
    return width, height, rate


def digest(buffer):
    return hashlib.sha256(buffer.extract_dup(0, buffer.get_size())).hexdigest()


def receive(source, *, frames=None, loop=False, realtime=True):
    source = Path(source).resolve(strict=True)
    if frames is not None and frames <= 0:
        raise ValueError("frame limit must be positive")
    width, height, rate = probe(source)
    Gst.init(None)
    for element in ("fdsrc", "rawvideoparse", "capsfilter", "appsink"):
        if Gst.ElementFactory.find(element) is None:
            raise RuntimeError(f"missing GStreamer element: {element}")

    command = [
        "ffmpeg", "-nostdin", "-hide_banner", "-loglevel", "error",
        "-threads", "2", "-filter_threads", "2",
    ]
    if realtime:
        command += ["-re"]
    if loop:
        command += ["-stream_loop", "-1"]
    command += [
        "-noautorotate", "-i", str(source), "-map", "0:v:0", "-an", "-sn", "-dn",
        "-vf", "scale=in_range=tv:out_range=tv:"
        "in_color_matrix=bt709:out_color_matrix=bt709,format=uyvy422",
        "-r", str(rate), "-fps_mode", "cfr", "-threads", "2",
    ]
    if frames is not None:
        command += ["-frames:v", str(frames)]
    command += ["-pix_fmt", "uyvy422", "-c:v", "rawvideo", "-f", "rawvideo", "pipe:1"]

    producer = subprocess.Popen(command, stdout=subprocess.PIPE)
    pipeline = None
    try:
        # PTS below belongs to this software CFR feed, not to SDI or MP4 timecode.
        pipeline = Gst.parse_launch(
            f"fdsrc fd={producer.stdout.fileno()} ! "
            f"rawvideoparse format=uyvy width={width} height={height} "
            f"framerate={rate.numerator}/{rate.denominator} colorimetry=bt709 ! "
            "video/x-raw,format=UYVY,colorimetry=bt709,"
            "interlace-mode=progressive,pixel-aspect-ratio=1/1 ! "
            "appsink name=frames sync=false max-buffers=2 drop=false wait-on-eos=false"
        )
        sink = pipeline.get_by_name("frames")
        if not isinstance(sink, GstApp.AppSink):
            raise RuntimeError("pipeline did not create an appsink")
        bus = pipeline.get_bus()
        if pipeline.set_state(Gst.State.PLAYING) == Gst.StateChangeReturn.FAILURE:
            raise RuntimeError("GStreamer could not start the input pipeline")

        count = 0
        first_hash = None
        last_buffer = None
        caps = None
        started = last_progress = time.monotonic()
        idle_timeout = max(10.0, 3 / float(rate))
        while True:
            sample = sink.try_pull_sample(Gst.SECOND)
            error = bus.pop_filtered(Gst.MessageType.ERROR)
            if error is not None:
                details, _ = error.parse_error()
                raise RuntimeError(f"GStreamer input failed: {details.message}")
            if sample is None:
                if sink.is_eos():
                    break
                if time.monotonic() - last_progress > idle_timeout:
                    raise TimeoutError(f"no video frame received for {idle_timeout:g} seconds")
                continue
            buffer = sample.get_buffer()
            if buffer.get_size() != width * height * 2:
                raise RuntimeError("unexpected UYVY frame size")
            expected_pts = count * Gst.SECOND * rate.denominator // rate.numerator
            # rawvideoparse may round each frame duration down to nanoseconds.
            if buffer.pts == Gst.CLOCK_TIME_NONE or abs(buffer.pts - expected_pts) > count:
                raise RuntimeError(
                    f"software-feed PTS is discontinuous: frame {count}, "
                    f"expected approximately {expected_pts}, received {buffer.pts}"
                )
            if first_hash is None:
                first_hash = digest(buffer)
                caps = sample.get_caps().to_string()
                print(
                    f"software feed ready: {width}x{height}, {rate} FPS, "
                    "UYVY BT.709 limited; video only, not physical SDI",
                    file=sys.stderr, flush=True,
                )
            last_buffer = buffer
            count += 1
            last_progress = time.monotonic()

        status = producer.wait(timeout=10)
        if status != 0:
            raise RuntimeError(f"FFmpeg exited with status {status}")
        if not count:
            raise RuntimeError("input produced no video frames")
        if frames is not None and count != frames:
            raise RuntimeError(f"expected {frames} frames, received {count}; use --loop for repeats")
        return {
            "transport": "software-ffmpeg-pipe",
            "physical_sdi": False,
            "audio": False,
            "width": width,
            "height": height,
            "fps": str(rate),
            "caps": caps,
            "frames": count,
            "bytes": count * width * height * 2,
            "first_pts_ns": 0,
            "last_pts_ns": last_buffer.pts,
            "first_sha256": first_hash,
            "last_sha256": digest(last_buffer),
            "elapsed_seconds": round(time.monotonic() - started, 3),
        }
    finally:
        if producer.poll() is None:
            producer.terminate()
        if pipeline is not None:
            pipeline.set_state(Gst.State.NULL)
        producer.stdout.close()
        try:
            producer.wait(timeout=5)
        except subprocess.TimeoutExpired:
            producer.kill()
            producer.wait()


def interrupted(_signum, _frame):
    raise KeyboardInterrupt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("--frames", type=int, help="require exactly this many decoded frames")
    parser.add_argument("--loop", action="store_true", help="repeat the source until interrupted")
    parser.add_argument("--unpaced", action="store_true", help="decode without realtime pacing")
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, interrupted)
    try:
        result = receive(
            args.source, frames=args.frames, loop=args.loop, realtime=not args.unpaced
        )
    except KeyboardInterrupt:
        return 130
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"software video input failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
