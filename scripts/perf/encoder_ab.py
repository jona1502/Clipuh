"""Encoder A/B harness for clipuh (Windows).

  python scripts/perf/encoder_ab.py extract CLIP.mp4 START COUNT OUT.bgra [BRIGHTNESS]
      Decode COUNT frames from START into a raw BGRA file for `encoder_ab`
      (BRIGHTNESS < 1 darkens them, for dark-scene coverage).

  python scripts/perf/encoder_ab.py bitstream STREAM.h264
      What the encoder really produced: SPS max_num_ref_frames, PPS entropy
      coding (CABAC/CAVLC), slices per frame, slice types, IDR spacing.

  python scripts/perf/encoder_ab.py run EXE FRAMES LABEL [encoder_ab args...]
      Run one variant: waits for "encoding started", samples the GPU engine
      counters of the bench process once per second with typeperf (each
      engine type summed over the process' instances, as Task Manager does),
      prints one JSON line with the counters and the bench's RESULT values.

  python scripts/perf/encoder_ab.py quality FRAMES STREAM.h264 [COUNT] [STRIDE]
      SSIM (luma, full resolution) and PSNR (luma) of every STRIDE-th of the
      first COUNT decoded frames against the source frames. Relative between variants: both sides
      go through the same decoder and the same colour conversion.

Needs numpy, opencv-python and scikit-image.
"""
import csv
import io
import json
import re
import statistics
import subprocess
import sys
import threading

import numpy as np


def extract(clip, start, count, out, brightness="1"):
    import cv2

    capture = cv2.VideoCapture(clip)
    capture.set(cv2.CAP_PROP_POS_FRAMES, int(start))
    frames = []
    for _ in range(int(count)):
        ok, frame = capture.read()
        if not ok:
            break
        if float(brightness) != 1.0:
            # Darkened copy of real footage for dark-scene coverage.
            frame = cv2.convertScaleAbs(frame, alpha=float(brightness))
        frames.append(cv2.cvtColor(frame, cv2.COLOR_BGR2BGRA))
    if not frames:
        sys.exit("no frames decoded")
    height, width = frames[0].shape[:2]
    with open(out, "wb") as file:
        file.write(np.array([width, height, len(frames)], dtype="<u4").tobytes())
        for frame in frames:
            file.write(frame.tobytes())
    print(f"{out}: {len(frames)} frames {width}x{height}")


def read_frames(path, limit=None):
    with open(path, "rb") as file:
        width, height, count = np.frombuffer(file.read(12), dtype="<u4")
        count = count if limit is None else min(count, limit)
        size = int(width) * int(height) * 4
        for _ in range(int(count)):
            yield np.frombuffer(file.read(size), dtype=np.uint8).reshape(int(height), int(width), 4)


def luma(bgr):
    # BT.709 luma of 8-bit BGR, as float.
    b, g, r = (bgr[..., i].astype(np.float32) for i in range(3))
    return 0.0722 * b + 0.7152 * g + 0.2126 * r


def quality(frames_path, stream, count=None, stride=3):
    import cv2
    from skimage.metrics import structural_similarity

    capture = cv2.VideoCapture(stream)
    ssims, psnrs = [], []
    for index, source in enumerate(read_frames(frames_path, None if count is None else int(count))):
        ok, decoded = capture.read()
        if not ok:
            break
        if index % int(stride):
            continue
        a, b = luma(source[..., :3]), luma(decoded)
        ssims.append(structural_similarity(a, b, data_range=255.0))
        mse = float(np.mean((a - b) ** 2))
        psnrs.append(99.0 if mse == 0 else 10 * np.log10(255.0**2 / mse))
    print(json.dumps({
        "stream": stream,
        "frames": len(ssims),
        "ssim_mean": round(statistics.fmean(ssims), 5),
        "ssim_min": round(min(ssims), 5),
        "ssim_p5": round(sorted(ssims)[len(ssims) // 20], 5),
        "psnr_mean": round(statistics.fmean(psnrs), 3),
        "psnr_min": round(min(psnrs), 3),
        "psnr_p5": round(sorted(psnrs)[len(psnrs) // 20], 3),
    }))


class Bits:
    def __init__(self, data):
        # Remove emulation prevention bytes (00 00 03).
        out, zeros = bytearray(), 0
        for byte in data:
            if zeros >= 2 and byte == 3:
                zeros = 0
                continue
            out.append(byte)
            zeros = zeros + 1 if byte == 0 else 0
        self.data, self.pos = bytes(out), 0

    def u(self, n):
        value = 0
        for _ in range(n):
            byte = self.data[self.pos >> 3]
            value = (value << 1) | ((byte >> (7 - (self.pos & 7))) & 1)
            self.pos += 1
        return value

    def ue(self):
        zeros = 0
        while self.u(1) == 0:
            zeros += 1
        return (1 << zeros) - 1 + (self.u(zeros) if zeros else 0)


def bitstream(stream):
    import collections
    data = open(stream, "rb").read()
    starts = [m.start() + 4 for m in re.finditer(b"\x00\x00\x00\x01", data)]
    ends = starts[1:] + [len(data) + 4]
    sps = pps = None
    frames, slices_per_frame, types, idr_frames = 0, collections.Counter(), collections.Counter(), []
    current = 0
    for start, end in zip(starts, ends):
        nal = data[start:end - 4]
        kind = nal[0] & 0x1F
        bits = Bits(nal[1:])
        if kind == 7 and sps is None:
            profile = bits.u(8); bits.u(16); bits.ue()
            if profile in (100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135):
                if bits.ue() == 3:
                    bits.u(1)
                bits.ue(); bits.ue(); bits.u(1)
                if bits.u(1):
                    sps = {"profile": profile, "max_num_ref_frames": "scaling lists (not parsed)"}
                    continue
            bits.ue()
            poc = bits.ue()
            if poc == 0:
                bits.ue()
            sps = {"profile": profile, "max_num_ref_frames": bits.ue() if poc != 1 else "poc type 1"}
        elif kind == 8 and pps is None:
            bits.ue(); bits.ue()
            pps = {"entropy": "CABAC" if bits.u(1) else "CAVLC"}
        elif kind in (1, 5):
            first_mb = bits.ue()
            slice_type = bits.ue() % 5
            if first_mb == 0:
                if frames:
                    slices_per_frame[current] += 1
                frames += 1
                current = 0
                types["PBI??"[slice_type]] += 1
                if kind == 5:
                    idr_frames.append(frames - 1)
            current += 1
    slices_per_frame[current] += 1
    gaps = sorted({b - a for a, b in zip(idr_frames, idr_frames[1:])})
    print(json.dumps({"stream": stream, "frames": frames, "sps": sps, "pps": pps,
                      "slices_per_frame": dict(slices_per_frame), "frame_types": dict(types),
                      "idr_gaps": gaps}))


def sample_gpu(pid, seconds):
    """Per-second sums of each engine type for `pid` and for the system."""
    out = subprocess.run(
        ["typeperf", r"\GPU Engine(*)\Utilization Percentage", "-si", "1", "-sc", str(seconds)],
        capture_output=True, text=True, errors="replace",
    ).stdout
    rows = [r for r in csv.reader(io.StringIO(out)) if len(r) > 1]
    header, data = rows[0], rows[1:]
    columns = []
    for name in header[1:]:
        match = re.search(r"pid_(\d+)_.*engtype_([^)\\]+)", name)
        columns.append((int(match.group(1)), match.group(2)) if match else None)
    process, system = {}, {}
    for row in data:
        if len(row) != len(header):
            continue
        sums_p, sums_s = {}, {}
        for column, cell in zip(columns, row[1:]):
            if column is None:
                continue
            try:
                value = float(cell)
            except ValueError:
                continue
            owner, engine = column
            sums_s[engine] = sums_s.get(engine, 0.0) + value
            if owner == pid:
                sums_p[engine] = sums_p.get(engine, 0.0) + value
        for target, sums in ((process, sums_p), (system, sums_s)):
            for engine, value in sums.items():
                target.setdefault(engine, []).append(value)
    def summary(table):
        return {e: {"median": round(statistics.median(v), 2), "mean": round(statistics.fmean(v), 2),
                    "min": round(min(v), 2), "max": round(max(v), 2)}
                for e, v in sorted(table.items()) if max(v) > 0.05}
    return {"process": summary(process), "system": summary(system), "samples": len(data)}


def run(exe, frames, label, *args):
    seconds = 20
    for arg in args:
        if arg.startswith("seconds="):
            seconds = int(arg.split("=", 1)[1])
    proc = subprocess.Popen([exe, f"frames={frames}", *args], stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, text=True)
    lines, gpu, started = [], {}, threading.Event()

    def reader():
        for line in proc.stdout:
            lines.append(line.rstrip())
            if "encoding started" in line:
                started.set()
    thread = threading.Thread(target=reader, daemon=True)
    thread.start()
    if not started.wait(300):
        proc.kill()
        sys.exit("bench did not start:\n" + "\n".join(lines))
    # Skip 3 s of warm-up, stop 2 s before the end.
    import time
    time.sleep(3)
    gpu = sample_gpu(proc.pid, max(seconds - 5, 3))
    proc.wait()
    thread.join()
    result = next((l for l in lines if l.startswith("RESULT")), "")
    values = dict(kv.split("=", 1) for kv in result.split()[1:] if "=" in kv)
    print(json.dumps({"label": label, "args": args, "gpu": gpu, "result": values,
                      "encoder": next((l for l in lines if l.startswith("encoder")), "")}))


def live(exe, label, *args):
    """Live replay (`replay_smoke`): sample the process' GPU engines for 20 s
    after an 8 s warm-up once ReplayStarted is printed."""
    import time
    proc = subprocess.Popen([exe, *args], stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                            text=True, errors="replace")
    lines, started = [], threading.Event()

    def reader():
        for line in proc.stdout:
            lines.append(line.rstrip())
            if "ReplayStarted" in line:
                started.set()
    thread = threading.Thread(target=reader, daemon=True)
    thread.start()
    if not started.wait(60):
        proc.kill()
        sys.exit("replay did not start:\n" + "\n".join(lines[-20:]))
    time.sleep(8)
    gpu = sample_gpu(proc.pid, 20)
    proc.wait()
    thread.join()
    status = [l for l in lines if " status " in l][-3:]
    keep = [l for l in lines if any(k in l for k in ("[end]", "clip ", "diagnostics", "outcomes", "ReplaySaveFailed"))]
    print(json.dumps({"label": label, "gpu": gpu, "status": status, "lines": keep}))


if __name__ == "__main__":
    command, *rest = sys.argv[1:]
    {"extract": extract, "quality": quality, "run": run, "live": live,
     "bitstream": bitstream}[command](*rest)
