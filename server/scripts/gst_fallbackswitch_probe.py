#!/usr/bin/env python3
"""Probe: does gst-plugins-rs' fallbackswitch give each output slot its own clean
frame when its CG frame is missing or late (#154)? Prints, per configuration,
the slots that were lost (repeated downstream), the missing-CG slots shown clean
and the CG slots replaced by clean.

Run in `nix develop .#media` (PyGObject and the livesync/fallbackswitch plugins):
    python3 server/scripts/gst_fallbackswitch_probe.py

The fallback pad gets every frame's clean feed at its capture; the main pad its
CG 30 ms later, except frame 10 (missing), 20-30 (a crashed worker) and 40
(later than the delay). 50 fps, program delay D = 100 ms.
"""
import time

import gi

gi.require_version("Gst", "1.0")
from gi.repository import Gst  # noqa: E402

T = 20_000_000
D = 100_000_000
FRAMES = 60
MISSING = {10, *range(20, 31)}
LATE = {40}
CAPS = "video/x-raw,format=GRAY8,width=16,height=16,framerate=50/1"


def run(restamp, timeout, latency):
    """Outputs (frame, cg) of one run. `restamp`: PTS = due time (capture + D),
    else PTS = capture time and fallbackswitch's `latency` covers D."""
    pipeline = Gst.parse_launch(
        f"appsrc name=main is-live=true format=time caps={CAPS} ! queue ! switch.sink_0 "
        f"appsrc name=clean is-live=true format=time caps={CAPS} "
        "! queue max-size-buffers=0 max-size-time=0 max-size-bytes=0 ! switch.sink_1 "
        f"fallbackswitch name=switch timeout={timeout} latency={latency} "
        "! appsink name=out sync=true async=false emit-signals=true max-buffers=0"
    )
    switch = pipeline.get_by_name("switch")
    switch.get_static_pad("sink_0").set_property("priority", 0)
    switch.get_static_pad("sink_1").set_property("priority", 1)
    outputs = []

    def on_sample(sink):
        buffer = sink.emit("pull-sample").get_buffer()
        _, info = buffer.map(Gst.MapFlags.READ)
        outputs.append((buffer.offset, info.data[0] == 200))
        buffer.unmap(info)
        return Gst.FlowReturn.OK

    pipeline.get_by_name("out").connect("new-sample", on_sample)
    pipeline.set_state(Gst.State.PLAYING)
    pipeline.get_state(Gst.CLOCK_TIME_NONE)
    clock, base, start = pipeline.get_clock(), pipeline.get_base_time(), 200_000_000
    events = [(n * T, "clean", n) for n in range(FRAMES)]
    events += [
        (n * T + (D if n in LATE else 0) + 30_000_000, "main", n)
        for n in range(FRAMES)
        if n not in MISSING
    ]
    for at, pad, n in sorted(events):
        while (left := base + start + at - clock.get_time()) > 0:
            time.sleep(min(left / 1e9, 0.002))
        buffer = Gst.Buffer.new_wrapped(bytes([200 if pad == "main" else 50]) * 256)
        buffer.pts = start + n * T + (D if restamp else 0)
        buffer.duration, buffer.offset = T, n
        pipeline.get_by_name(pad).emit("push-buffer", buffer)
    time.sleep(0.5)
    pipeline.set_state(Gst.State.NULL)
    return outputs


def main():
    Gst.init(None)
    no_cg = MISSING | LATE
    for name, restamp, timeout, latency in [
        ("PTS = due time, timeout 5 ms", True, 5_000_000, 0),
        ("PTS = due time, timeout 0", True, 0, 0),
        ("PTS = capture, latency = D, timeout 10 ms", False, 10_000_000, D),
    ]:
        outputs = dict(run(restamp, timeout, latency))
        lost = [n for n in range(FRAMES) if n not in outputs]
        print(f"{name}:")
        print(f"  slots lost (repeated): {lost}")
        print(f"  CG missing, shown clean: {sorted(n for n in no_cg if outputs.get(n) is False)}"
              f" of {sorted(no_cg)}")
        print(f"  CG available, shown clean: "
              f"{sorted(n for n, cg in outputs.items() if not cg and n not in no_cg)}")


if __name__ == "__main__":
    main()
