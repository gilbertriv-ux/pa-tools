#!/usr/bin/env python3
"""Check a saved session's speech transcript against a reference sentence.

Usage: check_transcript.py SESSION.scraft "reference sentence"

Fails (exit 1) unless the session holds exactly one transcript whose words reach a word error
rate below 0.25 against the reference, with start times that never go backwards and every word
inside the session's audio (end >= start >= 0). Standard library only.
"""
import json
import re
import sys


def words_of(text: str) -> list[str]:
    return re.sub(r"[^a-z0-9' ]", " ", text.lower()).split()


def edit_distance(a: list[str], b: list[str]) -> int:
    prev = list(range(len(b) + 1))
    for i, x in enumerate(a, 1):
        cur = [i]
        for j, y in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (x != y)))
        prev = cur
    return prev[-1]


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__)
        return 2
    session = json.load(open(sys.argv[1], encoding="utf-8"))
    sr = session["sample_rate"]
    transcripts = session.get("transcripts", [])
    if len(transcripts) != 1:
        print(f"FAIL: expected one transcript, found {len(transcripts)}")
        return 1
    words = transcripts[0]["words"]
    text = " ".join(w["text"] for w in words)
    ref, hyp = words_of(sys.argv[2]), words_of(text)
    wer = edit_distance(ref, hyp) / max(len(ref), 1)
    starts = [w["start"] / sr for w in words]
    ends = [w["end"] / sr for w in words]
    ordered = all(b >= a for a, b in zip(starts, starts[1:]))
    sane = all(0 <= s <= e for s, e in zip(starts, ends))
    print(f"words {len(words)}  WER {wer:.4f}  first {starts[0] if starts else -1:.2f}s  last {ends[-1] if ends else -1:.2f}s")
    print(f"text: {text}")
    if not words or wer >= 0.25 or not ordered or not sane:
        print(f"FAIL: words={len(words)} wer={wer:.4f} ordered={ordered} sane={sane}")
        return 1
    print("OK")
    return 0


if __name__ == "__main__":
    sys.exit(main())
