"""Judges each stress-test step and writes the summary (used by stress.sh).

  stress_report.py step OUT CPUS MEM CALLS STATS_FILE RESULT_JSON
  stress_report.py summary OUT

A step is healthy when every call finished, the agent answered every turn,
replies are not much slower than with one call, playback does not stutter
more than with one call, and RAM stays under 90 % of the limit.
"""

import json
import re
import sys
from pathlib import Path

SERVER = "voice-stress-voice-server-1"
UNITS = {"B": 1 / 2**20, "KiB": 1 / 1024, "kB": 1 / 1024, "MiB": 1, "MB": 1, "GiB": 1024, "GB": 1024}


def mib(text):
    m = re.match(r"([\d.]+)\s*([A-Za-z]+)", text.strip())
    return float(m.group(1)) * UNITS.get(m.group(2), 1) if m else 0.0


def usage(stats_file, name):
    """Peak and average CPU (in cores) and peak RAM (MiB) of one container."""
    cpu, mem = [], []
    for line in Path(stats_file).read_text().splitlines():
        parts = line.split(" ", 2)
        if len(parts) == 3 and parts[0] == name:
            cpu.append(float(parts[1].rstrip("%") or 0) / 100)
            mem.append(mib(parts[2].split("/")[0]))
    if not cpu:
        return {"cpu_peak": 0, "cpu_avg": 0, "mem_peak_mib": 0}
    return {
        "cpu_peak": round(max(cpu), 2),
        "cpu_avg": round(sum(cpu) / len(cpu), 2),
        "mem_peak_mib": round(max(mem)),
    }


def get(d, *path, default=0):
    for p in path:
        d = d.get(p) if isinstance(d, dict) else None
        if d is None:
            return default
    return d


def step(out, cpus, mem, calls, stats_file, result_json):
    out = Path(out)
    result = json.loads(result_json)
    server = usage(stats_file, SERVER)
    db = usage(stats_file, "voice-stress-postgres-1")
    mem_limit = mib(mem.replace("g", "GiB").replace("m", "MiB"))
    rows = out / "results.jsonl"
    earlier = [json.loads(l) for l in rows.read_text().splitlines()] if rows.exists() else []
    base = next((r for r in earlier if r["cpus"] == cpus and r["status"] == "OK"), None)

    problems = []
    if result.get("failed", 0):
        problems.append(f"{result['failed']} calls failed ({'; '.join(result.get('errors', [])[:2])})")
    if result.get("missed_replies", 0):
        problems.append(f"agent stayed silent {result['missed_replies']} times")
    p95 = get(result, "reply_ms", "p95")
    stutter = get(result, "stutter_ms_per_call", "p95")
    if base:
        b95 = get(base["result"], "reply_ms", "p95")
        if p95 > max(b95 * 1.5, b95 + 700):
            problems.append(f"replies slow: p95 {p95} ms vs {b95} ms with 1 call")
        bst = get(base["result"], "stutter_ms_per_call", "p95")
        if stutter > bst + 300:
            problems.append(f"audio stutters: {stutter} ms per call vs {bst} ms")
    if mem_limit and server["mem_peak_mib"] > 0.9 * mem_limit:
        problems.append(f"RAM nearly full: {server['mem_peak_mib']} MiB of {round(mem_limit)}")

    status = "OK" if not problems else "LIMIT"
    record = {
        "cpus": cpus, "mem": mem, "calls": int(calls), "status": status,
        "problems": problems, "server": server, "db": db, "result": result,
    }
    with rows.open("a") as f:
        f.write(json.dumps(record) + "\n")
    detail = (
        f"reply p50/p95 {get(result, 'reply_ms', 'p50')}/{p95} ms, "
        f"stutter p95 {stutter} ms/call, CPU peak {server['cpu_peak']}/{cpus} cores, "
        f"RAM peak {server['mem_peak_mib']} MiB"
    )
    print(f"{status}  {detail}" + ("" if not problems else "  <- " + "; ".join(problems)))


def summary(out):
    out = Path(out)
    rows = [json.loads(l) for l in (out / "results.jsonl").read_text().splitlines()]
    lines = ["# Stress test results", "",
             "Fake Sarvam + fake Plivo, offline, ₹0. Each call: greeting + 3 caller turns.", ""]
    for cpus in dict.fromkeys(r["cpus"] for r in rows):
        mine = [r for r in rows if r["cpus"] == cpus]
        ok = [r["calls"] for r in mine if r["status"] == "OK"]
        lines += [
            f"## {cpus} CPU / {mine[0]['mem']} RAM: handles **{max(ok) if ok else 0}** calls at once"
            + ("" if any(r["status"] != "OK" for r in mine) else " (did not reach its limit; try more calls)"),
            "",
            "| Calls | Status | Reply p50 / p95 | Stutter p95 / call | Server CPU peak | Server RAM peak | DB CPU peak |",
            "|---:|---|---|---|---|---|---|",
        ]
        for r in mine:
            res = r["result"]
            lines.append(
                f"| {r['calls']} | {r['status']} | {get(res, 'reply_ms', 'p50')} / {get(res, 'reply_ms', 'p95')} ms "
                f"| {get(res, 'stutter_ms_per_call', 'p95')} ms | {r['server']['cpu_peak']} / {cpus} cores "
                f"| {r['server']['mem_peak_mib']} MiB | {r['db']['cpu_peak']} |"
            )
            for p in r["problems"]:
                lines.append(f"|  | ↳ {p} | | | | | |")
        lines.append("")
    text = "\n".join(lines)
    (out / "summary.md").write_text(text)
    print("\n" + text)


if __name__ == "__main__":
    if sys.argv[1] == "step":
        step(*sys.argv[2:8])
    else:
        summary(sys.argv[2])
