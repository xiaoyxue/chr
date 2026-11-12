"""Summarize every scalar row and GPU timing; no failing rows are removed."""

import csv
import pathlib
import statistics

root = pathlib.Path("output/area-light-methods")
rows = list(csv.DictReader((root / "scalar.csv").open()))


def value(row, key):
    return float(row[key])


def label(row):
    return "/".join(row[k] for k in ("shape", "case", "roughness", "metal"))


def report(name, subset):
    print(name, "rows=", len(subset))
    for key in ("abs_error", "relative_error", "abs64", "relative64", "convergence_total",
                "polygon_spec_error", "fit_spec_error", "diffuse_error",
                "surface_cone_delta"):
        valid = [r for r in subset if r[key] not in ("undefined", "unavailable")]
        if valid:
            worst = max(valid, key=lambda r: abs(value(r, key)))
            print(f"  max_abs_{key}={value(worst, key):.9g} row={label(worst)}")
    print("  surface_delta_over_0.001=",
          sum(value(r, "convergence_total") > 0.001 for r in subset))


report("all", rows)
for shape in ("Rectangle", "Disk", "Sphere"):
    report(shape, [r for r in rows if r["shape"] == shape])
    # This is a declared candidate envelope, not a filter on measured errors.
    report(shape + " non-grazing/non-near roughness>=0.15", [
        r for r in rows if r["shape"] == shape and float(r["roughness"]) >= 0.15
        and r["case"] not in ("grazing", "horizon", "near", "inside")
    ])
gpu_path = root / "gpu.csv"
if gpu_path.exists():
    gpu = list(csv.DictReader(gpu_path.open()))
    for key in sorted({(r["shape"], r["lights"], r["sides"]) for r in gpu}):
        group = [r for r in gpu if (r["shape"], r["lights"], r["sides"]) == key]
        ms = [float(r["gpu_ms"]) for r in group]
        print("GPU", key, "samples=", len(ms), "median/min/max_ms=",
              statistics.median(ms), min(ms), max(ms), "max_scalar_delta=",
              max(float(r["max_scalar_delta"]) for r in group))
