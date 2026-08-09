#!/usr/bin/env python3
"""Score `ast-bro search` against a query set with known answers.

Ground truth is a line span plus a digest of the text it designates. Spans need
no language adapter, so a set can target any language ast-grep parses and any
symbol `ast-bro map` cannot see; the digest is what makes a span self-checking,
turning a corpus that moved under the set into a loud excluded row rather than
a plausible number. `--resolve` writes spans from the older symbol form, which
is still accepted and is the right choice for a set with no pinned revision.

Two scorings run together and they disagree on purpose.

IoU asks whether a result resembles the answer, thresholded by `--min-iou`.
The threshold is the part that matters: bare overlap rewards coarse chunking,
since a result spanning a whole 4000-line class overlaps every symbol in that
file by construction. Under bare overlap this repository's own pre-hierarchy
chunker scores 57% on pgjdbc against 50% for the current one; at IoU 0.3 it
scores 3% against 23%.

Cost asks whether a result carries the answer, or sits inside it, and charges
for everything read on the way. It catches what IoU calls a miss for being the
wrong zoom rather than the wrong place — a six-line method inside a 34-line
chunk — and it prices the coarse chunker without needing a threshold at all.

Usage:
    tests/eval/run_eval.py --repo ~/src/pgjdbc --queries tests/eval/queries/pgjdbc.tsv

Add `--verbose` for the per-query ranks and costs, which is what tells you
*which* query a change helped or hurt. Compare two variants by their per-query
rows rather than the summary lines: a one-query difference on a 30-query set is
noise, and four queries improving with none regressing is worth more than a
bigger number with churn underneath it.
"""

import argparse
import hashlib
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path


def run_binary(binary, repo, args, expect_json=True):
    """Run the ast-bro binary in `repo`, returning parsed stdout."""
    proc = subprocess.run(
        [binary, *args],
        cwd=repo,
        capture_output=True,
        text=True,
    )
    # A non-zero exit means the call itself was wrong (bad path, unknown
    # symbol). Never treat that as "no results" — the two are different, and
    # conflating them turns a broken harness into a plausible-looking score.
    if proc.returncode != 0:
        sys.exit(f"{binary} {' '.join(args)} failed:\n{proc.stderr.strip()}")
    if not expect_json:
        return proc.stdout
    return json.loads(proc.stdout)


def walk_declarations(declarations):
    for decl in declarations:
        yield decl
        yield from walk_declarations(decl.get("children", []))


def resolve_gold(binary, repo, path, symbol, near=None):
    """Line range of `symbol` in `path`, or None when it no longer exists.

    Returns `(start, end, ambiguous)`. A name can be declared several times in
    one file — overloads, or an interface method beside its implementation — and
    picking the first silently scores the query against whichever the parser
    happened to emit first. `near` disambiguates by choosing the declaration
    containing that line, or the nearest one.
    """
    doc = run_binary(binary, repo, ["map", path, "--json", "--compact"])
    matches = [
        decl
        for f in doc["files"]
        for decl in walk_declarations(f["declarations"])
        if decl.get("name") == symbol
    ]
    if not matches:
        return None
    if near is not None:
        matches.sort(
            key=lambda d: (
                not (d["start_line"] <= near <= d["end_line"]),
                abs(d["start_line"] - near),
            )
        )
        return matches[0]["start_line"], matches[0]["end_line"], False
    return matches[0]["start_line"], matches[0]["end_line"], len(matches) > 1


SPAN_RE = re.compile(r"^(\d+)(?::(\d+))?-(\d+)(?::(\d+))?$")


def parse_span(field):
    """`L1-L2`, or `L1:C1-L2:C2`. None when the field is a symbol name."""
    m = SPAN_RE.match(field)
    if not m:
        return None
    start_line, start_col, end_line, end_col = m.groups()
    return {
        "start_line": int(start_line),
        "end_line": int(end_line),
        "start_col": int(start_col) if start_col else None,
        "end_col": int(end_col) if end_col else None,
    }


def span_text(repo, path, span):
    """Exactly the text a span designates, or None if it runs past the file.

    Columns are 1-indexed and inclusive. The end column is applied before the
    start column so a single-line span slices correctly.
    """
    try:
        lines = (repo / path).read_text(errors="replace").splitlines()
    except OSError:
        return None
    if span["start_line"] < 1 or span["end_line"] > len(lines):
        return None
    body = lines[span["start_line"] - 1 : span["end_line"]]
    if span["end_col"] is not None:
        body[-1] = body[-1][: span["end_col"]]
    if span["start_col"] is not None:
        body[0] = body[0][span["start_col"] - 1 :]
    return "\n".join(body)


def digest_of(text):
    """Short content hash of an answer, which is what makes a span self-checking.

    A symbol-based set was checked by `ast-bro map`: rename the symbol and the
    query dropped out loudly. A line span has no such property on its own — bump
    the pinned revision and every span silently points at whatever now occupies
    those lines, and the eval still prints a plausible number. Hashing the text
    restores the guarantee without needing a language adapter, and it also
    catches a hand-edited line number, which `map` never did.
    """
    return hashlib.sha256(text.encode()).hexdigest()[:8]


def load_queries(path):
    """Queries plus the header directives, if the set pins a corpus."""
    rows, meta = [], {}
    for lineno, raw in enumerate(path.read_text().splitlines(), 1):
        if not raw.strip():
            continue
        if raw.startswith("#"):
            directive = raw.lstrip("#").strip()
            for key in ("repository", "revision"):
                prefix = f"{key}:"
                if directive.startswith(prefix):
                    meta[key] = directive[len(prefix):].strip()
            continue
        fields = raw.split("\t")
        if len(fields) < 3:
            sys.exit(f"{path}:{lineno}: expected at least 3 tab-separated fields")
        query, file, third = fields[0], fields[1], fields[2]
        span = parse_span(third)
        if span is not None:
            # query, path, span, label, digest — the span form needs no adapter,
            # so it can target any language and any symbol `map` cannot see.
            rows.append({
                "query": query,
                "path": file,
                "kind": "span",
                "span": span,
                "label": fields[3] if len(fields) > 3 else f"{file}:{third}",
                "digest": fields[4] if len(fields) > 4 else "-",
            })
            continue
        near = None
        if len(fields) > 3:
            try:
                near = int(fields[3])
            except ValueError:
                sys.exit(
                    f"{path}:{lineno}: 4th field must be a line number when the 3rd"
                    f" is a symbol, got {fields[3]!r}"
                )
        rows.append({
            "query": query,
            "path": file,
            "kind": "symbol",
            "symbol": third,
            "label": third,
            "near": near,
        })
    return rows, meta


def check_revision(repo, meta, allow_drift):
    """Refuse to score a pinned set against a different checkout.

    Two scores are only comparable when they come from the same corpus. A
    baseline taken on last month's master and a candidate taken on today's
    differ by both the change and a month of upstream commits, and nothing in
    the summary line says so.
    """
    wanted = meta.get("revision")
    if not wanted:
        return
    proc = subprocess.run(
        ["git", "-C", str(repo), "rev-parse", "HEAD"],
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        sys.exit(f"{repo} is not a git checkout, but the query set pins {wanted[:12]}")
    actual = proc.stdout.strip()
    if actual == wanted:
        return
    message = (
        f"{repo} is at {actual[:12]}, but the query set pins {wanted[:12]}.\n"
        f"  git -C {repo} checkout {wanted}"
    )
    if allow_drift:
        print(f"!! {message}\n!! --any-revision given; scores are not comparable")
    else:
        sys.exit(message)


def overlap_iou(hit, gold_start, gold_end):
    """Intersection over union of a result's line range and the answer's."""
    lo = max(hit["start_line"], gold_start)
    hi = min(hit["end_line"], gold_end)
    if hi < lo:
        return 0.0
    intersection = hi - lo + 1
    union = (
        max(hit["end_line"], gold_end) - min(hit["start_line"], gold_start) + 1
    )
    return intersection / union if union else 0.0


def coverage(hit, gold_start, gold_end):
    """Share of the answer's lines a result actually carries."""
    lo = max(hit["start_line"], gold_start)
    hi = min(hit["end_line"], gold_end)
    if hi < lo:
        return 0.0
    return (hi - lo + 1) / (gold_end - gold_start + 1)


def answer_cost(results, gold_path, gold_start, gold_end, count_outlines, min_coverage):
    """Rank of the first result carrying the answer, and what reading it cost.

    IoU asks whether a result *resembles* the answer. This asks whether it
    *contains* the answer, and how much text the reader got through first.
    The two disagree in both directions, which is the point.

    A six-line method packed beside its neighbours into one 34-line chunk
    scores IoU 0.2 and counts as a miss, even though the answer is on screen
    after 34 lines of reading. Ruby and Go are full of such methods, and their
    IoU scores measure method length more than they measure retrieval.

    A chunk spanning a whole 4400-line file also contains the answer. IoU
    rejects it via a threshold; this rejects it by charging for it, which
    needs no arbitrary constant — the budget it is compared against is a
    context window, which is a real quantity.

    A result answers when it carries most of the symbol, *or* when it lies
    inside the symbol. The second case is the `Part` chunk of a large method:
    a 19-line slice of a 165-line function covers 12% of it, yet it is the
    precise pointer, and charging it as a miss would punish exactly the
    precision the hierarchy was built for. Requiring coverage alone scored
    prometheus at 0 of 15 while IoU scored 2 — the targets there are long
    functions retrieved through their parts.

    What the pair rejects is a neighbouring chunk that grazes the answer by a
    line or two: it neither carries the symbol nor sits within it.

    Cost counts every result up to and including the one that answers, outline
    chunks included: the reader pays for whatever the search returned, whether
    or not it was allowed to count as the answer.
    """
    spent = 0
    for position, hit in enumerate(results, 1):
        spent += len(hit.get("content", ""))
        if not count_outlines and hit.get("kind") == "outline":
            continue
        if hit["path"] != gold_path:
            continue
        inside = hit["start_line"] >= gold_start and hit["end_line"] <= gold_end
        if inside or coverage(hit, gold_start, gold_end) >= min_coverage:
            return position, spent
    return None, spent


def rank_of_hit(results, gold_path, gold_start, gold_end, count_outlines, min_iou):
    """Rank of the first result that answers the query, or None.

    `min_iou` is what keeps the score honest. Bare overlap rewards coarse
    chunking: a result spanning a whole 4000-line class overlaps every symbol
    in the file by construction, so a chunker that never splits scores higher
    than one that points at the method. Requiring the ranges to actually
    resemble each other prices that in — "the answer is somewhere in this file"
    is not an answer.
    """
    for position, hit in enumerate(results, 1):
        if not count_outlines and hit.get("kind") == "outline":
            # An outline spans its whole region for the same reason.
            continue
        if hit["path"] != gold_path:
            continue
        iou = overlap_iou(hit, gold_start, gold_end)
        # `iou > 0` is required separately: at `min_iou = 0` the threshold
        # alone would accept a result that does not touch the answer at all,
        # which is file-level recall rather than the loosest line-level test.
        if iou > 0 and iou >= min_iou:
            return position
    return None


def resolve_set(binary, repo, path):
    """Print the query set in span form, resolving symbols through `ast-bro map`.

    `map` stops being a runtime dependency and becomes an authoring convenience:
    it writes the spans once, and scoring afterwards needs no adapter, so a
    language with no adapter — or a symbol an adapter cannot see, such as a C++
    header declaration or a Ruby `class << self` method — is still targetable by
    writing its span by hand.
    """
    for raw in path.read_text().splitlines():
        if not raw.strip() or raw.startswith("#"):
            print(raw)
            continue
        fields = raw.split("\t")
        query, file, third = fields[0], fields[1], fields[2]
        if parse_span(third) is not None:
            print(raw)
            continue
        near = int(fields[3]) if len(fields) > 3 else None
        resolved = resolve_gold(binary, repo, file, third, near)
        if resolved is None:
            print(f"# UNRESOLVED, span it by hand: {raw}")
            continue
        start, end, _ = resolved
        span = {"start_line": start, "end_line": end, "start_col": None, "end_col": None}
        text = span_text(repo, file, span)
        digest = digest_of(text) if text is not None else "-"
        print(f"{query}\t{file}\t{start}-{end}\t{third}\t{digest}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", required=True, type=Path, help="repository to search")
    parser.add_argument("--queries", required=True, type=Path, help="TSV query set")
    parser.add_argument("--binary", default=shutil.which("ast-bro") or "ast-bro")
    parser.add_argument("--top-k", type=int, default=10)
    parser.add_argument("--recall-at", type=int, default=5)
    parser.add_argument(
        "--count-outlines",
        action="store_true",
        help="let an outline hit count, scoring file-level rather than line-level recall",
    )
    parser.add_argument(
        "--min-iou",
        type=float,
        default=0.3,
        help="how much a result's line range must resemble the answer's (0 = bare overlap)",
    )
    parser.add_argument(
        "--any-revision",
        action="store_true",
        help="score even when the checkout differs from the pinned revision",
    )
    parser.add_argument(
        "--min-coverage",
        type=float,
        default=0.5,
        help="share of the answer a result must carry to count, for the cost metric",
    )
    parser.add_argument(
        "--budget",
        type=int,
        nargs="+",
        default=[8000, 32000],
        help="reading budgets in characters, for answered@budget",
    )
    parser.add_argument(
        "--resolve",
        action="store_true",
        help="print the set in span form (query, path, span, label, digest) and exit",
    )
    parser.add_argument("--verbose", action="store_true", help="print per-query ranks")
    args = parser.parse_args()

    # A cutoff wider than the result list reports a recall the search was never
    # asked for: the harness retrieves `--top-k` results and would still label
    # the number `recall@--recall-at`. The rest are ordinary bounds — a metric
    # that silently accepts a nonsensical setting produces a quotable number
    # from it.
    if args.top_k < 1:
        parser.error("--top-k must be positive")
    if args.recall_at < 1:
        parser.error("--recall-at must be positive")
    if args.recall_at > args.top_k:
        parser.error(f"--recall-at {args.recall_at} exceeds --top-k {args.top_k}")
    if not 0.0 <= args.min_iou <= 1.0:
        parser.error("--min-iou must be between 0 and 1")
    if not 0.0 <= args.min_coverage <= 1.0:
        parser.error("--min-coverage must be between 0 and 1")

    if not args.repo.is_dir():
        sys.exit(f"no such repository: {args.repo}")

    if args.resolve:
        resolve_set(args.binary, args.repo, args.queries)
        return

    queries, meta = load_queries(args.queries)
    check_revision(args.repo, meta, args.any_revision)
    hits, reciprocal_rank, rows, unresolved, ambiguous = 0, 0.0, [], [], []
    costs, stale, unpinned = [], [], []

    for q in queries:
        query, path, symbol = q["query"], q["path"], q["label"]
        if q["kind"] == "span":
            text = span_text(args.repo, path, q["span"])
            if text is None:
                unresolved.append(f"{path}:{symbol} (span runs past the file)")
                continue
            if q["digest"] == "-":
                unpinned.append(symbol)
            elif digest_of(text) != q["digest"]:
                stale.append(f"{path}:{symbol}")
                continue
            gold, is_ambiguous = [q["span"]["start_line"], q["span"]["end_line"]], False
        else:
            resolved = resolve_gold(args.binary, args.repo, path, q["symbol"], q["near"])
            if resolved is None:
                unresolved.append(f"{path}:{symbol}")
                continue
            *gold, is_ambiguous = resolved
        if is_ambiguous:
            ambiguous.append(symbol)
        payload = run_binary(
            args.binary,
            args.repo,
            ["search", query, "-k", str(args.top_k), "--json", "--compact"],
        )
        rank = rank_of_hit(
            payload["results"], path, *gold, args.count_outlines, args.min_iou
        )
        cost_rank, cost = answer_cost(
            payload["results"], path, *gold, args.count_outlines, args.min_coverage
        )
        if rank and rank <= args.recall_at:
            hits += 1
        if rank:
            reciprocal_rank += 1.0 / rank
        costs.append(cost if cost_rank else None)
        rows.append((symbol, rank, cost_rank, cost))

    scored = len(rows)
    if scored == 0:
        sys.exit("no queries resolved — is --repo the right checkout?")

    if args.verbose:
        for symbol, rank, cost_rank, cost in rows:
            iou_col = f"rank={rank if rank else 'miss'}"
            cost_col = (
                f"cost={cost} chars @rank {cost_rank}" if cost_rank else "cost=unanswered"
            )
            print(f"   {symbol:<30} {iou_col:<12} {cost_col}")
    if unresolved:
        # Loud, because a silently shrinking query set makes a score look
        # better than it is.
        print(f"!! {len(unresolved)} unresolved, excluded: {', '.join(unresolved)}")
    if stale:
        # The corpus and the set disagree about what lives at those lines. Loud
        # and excluded, because scoring them would compare against whatever code
        # happens to occupy the span now.
        print(
            f"!! {len(stale)} stale digest(s), excluded: {', '.join(stale)}"
            "\n!! re-run with --resolve, or fix the span by hand"
        )
    if unpinned:
        print(
            f"!! {len(unpinned)} span(s) carry no digest and are unverified:"
            f" {', '.join(unpinned)}"
        )
    if ambiguous:
        # Equally loud: an ambiguous answer can make a query unhittable at any
        # search quality, which reads as a miss rather than as a bad question.
        print(
            f"!! {len(ambiguous)} ambiguous, scored against the first declaration:"
            f" {', '.join(ambiguous)}"
        )
        print("!! add a 4th tab-separated field with the intended line to pin them")

    print(
        f"recall@{args.recall_at} = {hits}/{scored} = {100 * hits / scored:.0f}%"
        f"   MRR@{args.top_k} = {reciprocal_rank / scored:.3f}"
        f"   (min-iou {args.min_iou})"
    )

    answered = [c for c in costs if c is not None]
    budgets = "   ".join(
        f"answered@{b // 1000}k = {sum(1 for c in answered if c <= b)}/{scored}"
        f" = {100 * sum(1 for c in answered if c <= b) / scored:.0f}%"
        for b in sorted(args.budget)
    )
    median = sorted(answered)[len(answered) // 2] if answered else None
    print(
        f"{budgets}   median cost = {median if median is not None else 'n/a'} chars"
        f"   (min-coverage {args.min_coverage})"
    )


if __name__ == "__main__":
    main()
