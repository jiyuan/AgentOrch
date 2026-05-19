#!/usr/bin/env python3
"""
token_counter.py — Parse per-call token logging records from trace JSONL and session JSONL.

IMPORTANT: This script is preserved for future use when:
1. The AgentOS runtime logs `token_counts` or `usage` fields on LLM spans and assistant transcript items.
2. `python3` is added to the shell allowlist.

Until both conditions are met, the audit-skill SKILL.md instructs the LLM to estimate
tokens from character counts rather than running this script.

Expected record formats (not currently present in workspace data):

Trace span format (span with kind="llm"):
{
  "record_type": "span",
  "span": {
    "kind": "llm",
    "fields": {
      "token_counts": {
        "input_tokens": 1234,
        "output_tokens": 567,
        "total_tokens": 1801,
        "cache_hit_tokens": 800,
        "cache_miss_tokens": 434
      }
    }
  }
}

Session transcript format (in metadata of assistant messages):
{
  "event": "transcript_item",
  "message": {
    "role": "assistant",
    "metadata": {
      "token_counts": {
        "input_tokens": ...,
        "output_tokens": ...,
        "total_tokens": ...,
        "cache_hit_tokens": ...,
        "cache_miss_tokens": ...
      }
    }
  }
}

Gateway log format:
trace: run=1, plan=N, llm=M  (where llm=M is the model call count)

Usage:
  python3 token_counter.py <trace-file> [session-file...]
  python3 token_counter.py --summarize <trace-file> [session-file...]
"""

import json
import sys
from collections import defaultdict


def parse_trace_file(path):
    """Parse a trace JSONL file and extract per-call token records from llm spans."""
    counters = {
        "model_calls": 0,
        "input_tokens": 0,
        "output_tokens": 0,
        "total_tokens": 0,
        "cache_hit_tokens": 0,
        "cache_miss_tokens": 0,
        "from_trace_fields": False,
        "from_estimated": False,
    }
    try:
        with open(path, "r", encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    record = json.loads(line)
                except json.JSONDecodeError:
                    continue
                # Check for span records with kind="llm"
                if record.get("record_type") == "span":
                    span = record.get("span", {})
                    if span.get("kind") == "llm":
                        counters["model_calls"] += 1
                        fields = span.get("fields", {})
                        tc = fields.get("token_counts", fields.get("usage", {}))
                        if isinstance(tc, dict) and tc.get("input_tokens") is not None:
                            counters["input_tokens"] += tc.get("input_tokens", 0)
                            counters["output_tokens"] += tc.get("output_tokens", 0)
                            counters["total_tokens"] += tc.get("total_tokens", tc.get("input_tokens", 0) + tc.get("output_tokens", 0))
                            counters["cache_hit_tokens"] += tc.get("cache_hit_tokens", 0)
                            counters["cache_miss_tokens"] += tc.get("cache_miss_tokens", 0)
                            counters["from_trace_fields"] = True
                # Check for event records with token data
                if record.get("record_type") == "event":
                    event = record.get("event", {})
                    fields = event.get("fields", {})
                    tc = fields.get("token_counts", fields.get("usage", {}))
                    if isinstance(tc, dict) and tc.get("input_tokens") is not None:
                        counters["input_tokens"] += tc.get("input_tokens", 0)
                        counters["output_tokens"] += tc.get("output_tokens", 0)
                        counters["total_tokens"] += tc.get("total_tokens", tc.get("input_tokens", 0) + tc.get("output_tokens", 0))
                        counters["cache_hit_tokens"] += tc.get("cache_hit_tokens", 0)
                        counters["cache_miss_tokens"] += tc.get("cache_miss_tokens", 0)
                        counters["from_trace_fields"] = True
    except FileNotFoundError:
        return counters, f"ERROR: file not found: {path}"
    except Exception as e:
        return counters, f"ERROR: {e}"
    return counters, None


def parse_session_file(path):
    """Parse a session JSONL file and extract per-call token records from assistant message metadata."""
    counters = {
        "model_calls": 0,
        "input_tokens": 0,
        "output_tokens": 0,
        "total_tokens": 0,
        "cache_hit_tokens": 0,
        "cache_miss_tokens": 0,
        "from_session_fields": False,
        "from_estimated": False,
    }
    try:
        with open(path, "r", encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    record = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if record.get("event") == "transcript_item":
                    msg = record.get("message", {})
                    if msg.get("role") == "assistant":
                        metadata = msg.get("metadata", {})
                        tc = metadata.get("token_counts", metadata.get("usage", {}))
                        if isinstance(tc, dict) and tc.get("input_tokens") is not None:
                            counters["model_calls"] += 1
                            counters["input_tokens"] += tc.get("input_tokens", 0)
                            counters["output_tokens"] += tc.get("output_tokens", 0)
                            counters["total_tokens"] += tc.get("total_tokens", tc.get("input_tokens", 0) + tc.get("output_tokens", 0))
                            counters["cache_hit_tokens"] += tc.get("cache_hit_tokens", 0)
                            counters["cache_miss_tokens"] += tc.get("cache_miss_tokens", 0)
                            counters["from_session_fields"] = True
                        else:
                            # Estimate from content if no exact token counts
                            content = msg.get("content", "")
                            if content:
                                estimated = max(1, len(content) // 4)
                                counters["input_tokens"] += estimated
                                counters["from_estimated"] = True
    except FileNotFoundError:
        return counters, f"ERROR: file not found: {path}"
    except Exception as e:
        return counters, f"ERROR: {e}"
    return counters, None


def merge_counters(*counter_list):
    """Merge multiple counter dicts into one."""
    merged = defaultdict(int)
    for c in counter_list:
        for k, v in c.items():
            if isinstance(v, bool):
                merged[k] = merged.get(k, False) or v
            else:
                merged[k] += v
    return dict(merged)


def main():
    args = sys.argv[1:]
    summarize = False

    if not args:
        print("Usage: token_counter.py [--summarize] <trace-file> [session-file...]")
        sys.exit(1)

    if args[0] == "--summarize":
        summarize = True
        args = args[1:]

    trace_file = args[0] if args else None
    session_files = args[1:]

    all_counters = []
    errors = []

    if trace_file:
        tc, err = parse_trace_file(trace_file)
        if err:
            errors.append(err)
        else:
            all_counters.append(tc)

    for sf in session_files:
        sc, err = parse_session_file(sf)
        if err:
            errors.append(err)
        else:
            all_counters.append(sc)

    if not all_counters:
        print(json.dumps({
            "model_calls": 0,
            "input_tokens": 0,
            "output_tokens": 0,
            "total_tokens": 0,
            "cache_hit_tokens": 0,
            "cache_miss_tokens": 0,
            "exact": False,
            "estimated": True,
            "errors": errors,
            "note": "No token logging records found; all values are zero."
        }, indent=2))
        sys.exit(0)

    merged = merge_counters(*all_counters)
    exact = merged.pop("from_trace_fields", False) or merged.pop("from_session_fields", False)
    estimated = merged.pop("from_estimated", False)

    result = {
        **merged,
        "exact": exact,
        "estimated": estimated,
        "errors": errors if errors else None,
        "note": None,
    }

    if not exact:
        result["note"] = "Exact token counts unavailable; values derived from character-based estimation (ceil(char_count/4))."

    if summarize:
        print(f"Model calls: {result['model_calls']}")
        if exact:
            print(f"Input tokens: {result['input_tokens']}")
            print(f"Output tokens: {result['output_tokens']}")
            print(f"Total tokens: {result['total_tokens']}")
            print(f"Cache hit tokens: {result['cache_hit_tokens']}")
            print(f"Cache miss tokens: {result['cache_miss_tokens']}")
        else:
            print(f"Input tokens: ~{result['input_tokens']} estimated")
            print(f"Output tokens: ~{result['output_tokens']} estimated")
            print(f"Total tokens: ~{result['total_tokens']} estimated")
        print(f"Exact: {exact}")
        if errors:
            for e in errors:
                print(f"Warning: {e}")
    else:
        print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
