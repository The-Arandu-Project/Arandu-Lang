#!/usr/bin/env python3
"""Reproducible benchmark harness for parallelFold and multi-core scaling.

Measures wall clock, user time, system time, and max RSS across sequential and
varying worker pool sizes (1, 2, 4, 8, 16). Validates deterministic parity
across worker counts and generates structured JSON, CSV, and Markdown reports.
"""

import argparse
import csv
import json
import os
import platform
import resource
import subprocess
import sys
import time


def parse_args():
    parser = argparse.ArgumentParser(
        description="Benchmark parallel scaling and verify deterministic output."
    )
    parser.add_argument(
        "--bin",
        required=True,
        help="Path to executable under test (e.g. pypor or compiled Arandu benchmark).",
    )
    parser.add_argument(
        "--corpus",
        required=True,
        help="Path to input corpus or target directory to process.",
    )
    parser.add_argument(
        "--runs",
        type=int,
        default=3,
        help="Number of measured runs per configuration (default: 3).",
    )
    parser.add_argument(
        "--workers",
        nargs="+",
        type=int,
        default=[1, 2, 4, 8, 16],
        help="List of worker thread counts to test (default: 1 2 4 8 16).",
    )
    parser.add_argument(
        "--json-out",
        help="Optional path to output structured JSON results.",
    )
    parser.add_argument(
        "--csv-out",
        help="Optional path to output raw samples in CSV format.",
    )
    parser.add_argument(
        "--md-out",
        help="Optional path to output Markdown summary table.",
    )
    return parser.parse_args()


def run_single(binary: str, corpus: str, extra_args: list[str]):
    cmd = [binary, corpus] + extra_args
    start_wall = time.perf_counter()
    usage_start = resource.getrusage(resource.RUSAGE_CHILDREN)
    proc = subprocess.run(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    end_wall = time.perf_counter()
    usage_end = resource.getrusage(resource.RUSAGE_CHILDREN)

    wall_s = end_wall - start_wall
    user_s = usage_end.ru_utime - usage_start.ru_utime
    sys_s = usage_end.ru_stime - usage_start.ru_stime
    rss_kib = usage_end.ru_maxrss

    if proc.returncode != 0:
        sys.stderr.write(f"ERROR: command failed: {' '.join(cmd)}\n")
        sys.stderr.write(proc.stderr)
        sys.exit(proc.returncode)

    return {
        "wall_s": wall_s,
        "user_s": user_s,
        "sys_s": sys_s,
        "rss_kib": rss_kib,
        "stdout": proc.stdout,
    }


def normalize_output(text: str) -> str:
    lines = [
        line.strip()
        for line in text.strip().splitlines()
        if not line.strip().startswith("workers:")
    ]
    return "\n".join(lines)


def main():
    args = parse_args()
    if not os.path.exists(args.bin):
        sys.stderr.write(f"Binary not found: {args.bin}\n")
        sys.exit(1)
    if not os.path.exists(args.corpus):
        sys.stderr.write(f"Corpus not found: {args.corpus}\n")
        sys.exit(1)

    print(f"Host: {platform.node()} ({platform.system()} {platform.machine()})")
    print(f"Binary: {args.bin}")
    print(f"Corpus: {args.corpus}")
    print(f"Configurations: sequential, {args.workers} workers; {args.runs} runs each")

    # 1. Warmup run to warm page cache
    print("\n[1/3] Running preflight warmup...", flush=True)
    warmup = run_single(args.bin, args.corpus, ["--workers", "4"])
    print(f"Warmup completed in {warmup['wall_s']:.2f}s")
    baseline_norm = normalize_output(warmup["stdout"])

    # Build schedule: alternating forward/reverse order to mitigate thermal/cache bias
    configs = [("seq", ["--seq"])]
    for w in args.workers:
        configs.append((f"w{w}", ["--workers", str(w)]))

    print("\n[2/3] Collecting benchmark samples...", flush=True)
    samples = {c[0]: [] for c in configs}
    raw_records = []

    for round_idx in range(args.runs):
        ordered_configs = configs if round_idx % 2 == 0 else list(reversed(configs))
        print(f"--- Round {round_idx + 1}/{args.runs} ---", flush=True)
        for name, cmd_args in ordered_configs:
            res = run_single(args.bin, args.corpus, cmd_args)

            # Strict determinism validation: semantic output must be identical
            current_norm = normalize_output(res["stdout"])
            if current_norm != baseline_norm:
                sys.stderr.write(
                    f"ERROR: Non-deterministic output detected in config {name}!\n"
                )
                sys.exit(2)

            samples[name].append(res)
            raw_records.append({
                "round": round_idx + 1,
                "config": name,
                "wall_s": res["wall_s"],
                "user_s": res["user_s"],
                "sys_s": res["sys_s"],
                "rss_kib": res["rss_kib"],
            })
            print(
                f"  {name:5s}: wall={res['wall_s']:.3f}s | user={res['user_s']:.3f}s | "
                f"sys={res['sys_s']:.3f}s | rss={res['rss_kib'] / 1024:.1f} MiB",
                flush=True,
            )

    # 3. Analyze summary statistics
    print("\n[3/3] Analysis & Summary Statistics:", flush=True)
    summary = {}
    seq_median = sorted([s["wall_s"] for s in samples["seq"]])[len(samples["seq"]) // 2]

    print("\n| Configuration | Samples | Median Wall | Min - Max | Speedup | Median RSS |")
    print("| :--- | ---: | ---: | ---: | ---: | ---: |")

    md_rows = []
    for name, _ in configs:
        walls = sorted([s["wall_s"] for s in samples[name]])
        rsss = sorted([s["rss_kib"] for s in samples[name]])
        median_wall = walls[len(walls) // 2]
        min_wall = walls[0]
        max_wall = walls[-1]
        median_rss = rsss[len(rsss) // 2]
        speedup = seq_median / median_wall if median_wall > 0 else 1.0

        summary[name] = {
            "median_wall_s": median_wall,
            "min_wall_s": min_wall,
            "max_wall_s": max_wall,
            "speedup": speedup,
            "median_rss_kib": median_rss,
        }

        row = (
            f"| {name:13s} | {len(walls):7d} | {median_wall:9.3f} s | "
            f"{min_wall:.3f}–{max_wall:.3f} s | {speedup:6.2f}× | {median_rss / 1024:8.1f} MiB |"
        )
        print(row)
        md_rows.append(row)

    # Output files
    if args.json_out:
        with open(args.json_out, "w", encoding="utf-8") as f:
            json.dump(
                {
                    "system": {
                        "node": platform.node(),
                        "system": platform.system(),
                        "release": platform.release(),
                        "machine": platform.machine(),
                    },
                    "binary": args.bin,
                    "corpus": args.corpus,
                    "summary": summary,
                    "samples": samples,
                },
                f,
                indent=2,
            )
        print(f"\nJSON report written to: {args.json_out}")

    if args.csv_out:
        with open(args.csv_out, "w", newline="", encoding="utf-8") as f:
            writer = csv.DictWriter(
                f, fieldnames=["round", "config", "wall_s", "user_s", "sys_s", "rss_kib"]
            )
            writer.writeheader()
            writer.writerows(raw_records)
        print(f"CSV records written to: {args.csv_out}")

    if args.md_out:
        with open(args.md_out, "w", encoding="utf-8") as f:
            f.write("# Parallel Scaling Benchmark Report\n\n")
            f.write(f"- **Host:** `{platform.node()}` ({platform.system()} {platform.machine()})\n")
            f.write(f"- **Binary:** `{args.bin}`\n")
            f.write(f"- **Corpus:** `{args.corpus}`\n\n")
            f.write("| Configuration | Samples | Median Wall | Min - Max | Speedup | Median RSS |\n")
            f.write("| :--- | ---: | ---: | ---: | ---: | ---: |\n")
            for r in md_rows:
                f.write(r + "\n")
        print(f"Markdown report written to: {args.md_out}")


if __name__ == "__main__":
    main()
