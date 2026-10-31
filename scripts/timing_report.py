#!/usr/bin/env python3
"""Explain where a gongga turn spent time, from its structured event ledger.

    python scripts/timing_report.py <gongga-home> [--json report.json] [--all]

No provider request is made. `--all` prints every attempt and tool; the default
prints the twelve slowest of each. `--json` includes every row, not only those.
"""

from __future__ import annotations

import argparse
import json
import sqlite3
from collections import Counter
from pathlib import Path


def events(home: Path, kind: str) -> list[dict]:
    connection = sqlite3.connect(f"file:{home / 'gongga.db'}?mode=ro", uri=True)
    try:
        rows = connection.execute(
            "select payload from events where kind = ? order by id", (kind,)
        ).fetchall()
    finally:
        connection.close()
    return [json.loads(row[0]) for row in rows]


def millis(value) -> float:
    return round((value or 0) / 1000, 3)


def usage(attempt: dict, key: str):
    value = attempt.get("usage") or {}
    return value.get(key)


def cached(attempt: dict):
    return (attempt.get("usage") or {}).get("input_tokens_details", {}).get("cached_tokens")


def reasoning_tokens(attempt: dict):
    return (attempt.get("usage") or {}).get("output_tokens_details", {}).get("reasoning_tokens")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("home", type=Path)
    parser.add_argument("--json", type=Path, dest="json_path")
    parser.add_argument("--all", action="store_true")
    arguments = parser.parse_args()

    home = arguments.home
    turns = events(home, "turn_end")
    steps = events(home, "step_timing")
    attempts = events(home, "llm_attempt")
    tools = events(home, "tool_timing")
    old_usage = events(home, "usage")
    outcomes = Counter(attempt.get("outcome") or "unknown" for attempt in attempts)

    step_rows = []
    for step in steps:
        key = (step.get("turn"), step.get("step"))
        own_attempts = [a for a in attempts if (a.get("turn"), a.get("step")) == key]
        own_tools = [t for t in tools if (t.get("turn"), t.get("step")) == key]
        llm_ms = step.get("llm_ms") or 0
        other_ms = max(
            0,
            (step.get("elapsed_ms") or 0)
            - (step.get("prompt_ms") or 0)
            - (step.get("source_ms") or 0)
            - llm_ms,
        )
        step_rows.append(
            {
                **step,
                "attempts": len(own_attempts),
                "attempt_elapsed_ms": sum(a.get("elapsed_ms") or 0 for a in own_attempts),
                "tool_elapsed_ms": sum(t.get("elapsed_ms") or 0 for t in own_tools),
                "other_ms": other_ms,
            }
        )

    attempt_rows = []
    for attempt in attempts:
        input_tokens = usage(attempt, "input_tokens")
        cached_tokens = cached(attempt)
        attempt_rows.append(
            {
                **attempt,
                "process_ms": (attempt.get("process_total_us") or 0) / 1000,
                "uncached_input_tokens": input_tokens - cached_tokens
                if input_tokens is not None and cached_tokens is not None
                else None,
                "reasoning_tokens": reasoning_tokens(attempt),
            }
        )

    produced = {
        "home": str(home),
        "summary": {
            "turns": len(turns),
            "turn_elapsed_ms": sum(turn.get("elapsed_ms") or 0 for turn in turns),
            "steps": len(steps),
            "step_elapsed_ms": sum(step.get("elapsed_ms") or 0 for step in steps),
            "prompt_ms": sum(step.get("prompt_ms") or 0 for step in steps),
            "source_ms": sum(step.get("source_ms") or 0 for step in steps),
            "llm_ms": sum(step.get("llm_ms") or 0 for step in steps),
            "attempts": len(attempts),
            "attempt_elapsed_ms": sum(attempt.get("elapsed_ms") or 0 for attempt in attempts),
            "attempt_process_ms": sum(attempt.get("process_total_us") or 0 for attempt in attempts) / 1000,
            "raw_bytes": sum(attempt.get("raw_bytes") or 0 for attempt in attempts),
            "sse_events": sum(attempt.get("sse_events") or 0 for attempt in attempts),
            "tools": len(tools),
            "tool_elapsed_ms": sum(tool.get("elapsed_ms") or 0 for tool in tools),
            "outcomes": dict(outcomes),
            "legacy_usage_rows": len(old_usage),
        },
        "steps": step_rows,
        "attempts": attempt_rows,
        "tools": tools,
    }
    if arguments.json_path:
        arguments.json_path.write_text(json.dumps(produced, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    summary = produced["summary"]
    print("summary")
    for key, value in summary.items():
        if key.endswith("_ms"):
            print(f"  {key:24} {millis(value):10.3f} s")
        else:
            print(f"  {key:24} {value}")

    print("\nslow steps")
    print("step  turn   total_s prompt_s source_s    llm_s  attempts tools_s  other_s outcome")
    selected_steps = sorted(step_rows, key=lambda row: row.get("elapsed_ms") or 0, reverse=True)
    for row in selected_steps if arguments.all else selected_steps[:12]:
        print(
            f"{row.get('step'):>5} {row.get('turn'):>5} "
            f"{millis(row.get('elapsed_ms')):>9.3f} {millis(row.get('prompt_ms')):>9.3f} "
            f"{millis(row.get('source_ms')):>9.3f} {millis(row.get('llm_ms')):>8.3f} "
            f"{row.get('attempts'):>9} {millis(row.get('tool_elapsed_ms')):>8.3f} "
            f"{millis(row.get('other_ms')):>8.3f} {row.get('outcome')}"
        )

    print("\nslow attempts")
    print("step  outcome        total_s build_s open_s headers_s first_r_s last_r_s first_o_s last_o_s process_s raw_KiB events input output cached")
    selected_attempts = sorted(attempt_rows, key=lambda row: row.get("elapsed_ms") or 0, reverse=True)
    for row in selected_attempts if arguments.all else selected_attempts[:12]:
        print(
            f"{row.get('step'):>5} {str(row.get('outcome')):14.14} "
            f"{millis(row.get('elapsed_ms')):>8.3f} {millis(row.get('build_head_ms')):>8.3f} "
            f"{millis(row.get('transcript_open_ms')):>7.3f} {millis(row.get('headers_ms')):>10.3f} "
            f"{millis(row.get('first_reasoning_delta_ms')):>10.3f} {millis(row.get('last_reasoning_delta_ms')):>9.3f} "
            f"{millis(row.get('first_output_delta_ms')):>10.3f} {millis(row.get('last_output_delta_ms')):>9.3f} "
            f"{millis(row.get('process_ms')):>9.3f} {(row.get('raw_bytes') or 0) / 1024:>8.1f} "
            f"{row.get('sse_events') or 0:>6} {usage(row, 'input_tokens') or 0:>6} "
            f"{usage(row, 'output_tokens') or 0:>6} {cached(row) or 0:>6}"
        )

    print("\nslow tools")
    print("step  tool                 total_s    run_s attach_s  chars status")
    selected_tools = sorted(tools, key=lambda row: row.get("elapsed_ms") or 0, reverse=True)
    for row in selected_tools if arguments.all else selected_tools[:12]:
        print(
            f"{row.get('step'):>5} {str(row.get('tool')):20.20} "
            f"{millis(row.get('elapsed_ms')):>8.3f} {millis(row.get('run_ms')):>8.3f} "
            f"{millis(row.get('attach_ms')):>9.3f} {row.get('output_chars') or 0:>6} {row.get('status')}"
        )


if __name__ == "__main__":
    main()
