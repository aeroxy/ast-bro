# Retrieval eval

Measures whether `ast-bro search` returns the code a query is about. It exists
because search quality is not something you can eyeball: several changes that
looked obviously right measured flat, one that looked wrong measured well, and
a non-determinism bug lived in the ranking code until this harness surfaced it.

## Running it

```bash
git clone https://github.com/pgjdbc/pgjdbc ~/src/pgjdbc
git -C ~/src/pgjdbc checkout 29bbe0268c1e55cc2037fa171d61dcdafca39a7a
cargo build --release
tests/eval/run_eval.py \
  --repo ~/src/pgjdbc \
  --queries tests/eval/queries/pgjdbc.tsv \
  --binary target/release/ast-bro
```

```text
recall@5 = 7/30 = 23%   MRR@10 = 0.226   (min-iou 0.3)
answered@8k = 14/30 = 47%   answered@32k = 17/30 = 57%   median cost = 2246 chars   (min-coverage 0.5)
```

The revision is pinned in the query set and checked before scoring, because two
scores are comparable only when they come from the same corpus. Measuring a
change on today's master against a baseline from last month compares the change
and a month of upstream commits at once, and the summary line does not say so.
`--any-revision` overrides the check and prints a warning.

`--verbose` prints the rank of each query. That is the output to compare
between two variants; see *Reading a result* below.

`queries/self.tsv` runs against this repository, so it needs no external
checkout:

```bash
tests/eval/run_eval.py --repo . --queries tests/eval/queries/self.tsv \
  --binary target/release/ast-bro
```

It is a smoke test, not evidence. Twenty Rust queries over a small repository
cannot separate two ranking variants, and its absolute score reads low for a
reason unrelated to quality: a near-miss counts as a miss, and on a small
codebase the top hit is often the chunk immediately before the answer. Judge a
change on a pinned set.

## Query sets

One query per line, tab-separated. The answer is a **line span**:

```text
<natural-language query>	<file>	<span>	<label>	<digest>
```

`<span>` is `startLine-endLine`, or `startLine:startColumn-endLine:endColumn`
when the answer is finer than whole lines. Columns are 1-indexed and inclusive.
`<label>` names the answer for the per-query output and is not scored.

Spans need no language adapter, which is the point: `ast-bro map` covers
thirteen of the twenty-seven languages ast-grep parses, and misses symbols even
within those — a declaration in a C++ header, a Ruby method inside
`class << self`, a C# indexer, anything under `#if DEBUG`. A span can target
all of them.

`<digest>` is what keeps a span honest. It is the first eight hex characters of
the SHA-256 of the text the span designates. Line numbers alone rot silently:
bump the pinned revision and every span points at whatever now occupies those
lines, while the summary still prints a plausible number. The digest turns that
into a loud, excluded row, and it also catches a hand-edited line number, which
the older symbol form never did. Write `-` to leave a span unverified; the
harness lists every unverified row.

Generate a set rather than counting lines by hand:

```bash
tests/eval/run_eval.py --repo ~/src/pgjdbc --queries old.tsv --resolve > new.tsv
```

`--resolve` reads the older symbol form, asks `ast-bro map` where each symbol
lives, and prints spans with digests. So `map` stays useful for authoring
without being a runtime dependency, and a target it cannot see is written by
hand.

The **symbol form is still accepted** and is the right choice for a set with no
pinned revision, where a span would go stale on every edit — `self.tsv` runs
against the working tree for exactly that reason and stays symbol-based:

```text
<natural-language query>	<file>	<symbol>	[line]
```

A name can be declared more than once in a file, and then the first declaration
is not necessarily the one the query means: `rollback` in `PgConnection` is both
`rollback()` and `rollback(Savepoint)`, and a query about savepoints scored
against the first can never be a hit at any search quality. The optional fourth
field pins the intended line, and the harness warns for every unpinned
ambiguous symbol.

Write queries the way someone describes behaviour they cannot yet locate
("release a savepoint on the server"), not the way they would grep
("sendReleaseSavepoint"). A query that repeats the symbol name measures the
lexical half only, and the lexical half was never the weak one.

## Two scorings, and why both

**IoU** — a result counts when it comes from the right file and its line range
resembles the answer's: intersection over union at or above `--min-iou`,
default 0.3. Outline chunks are skipped, for the same reason an outline spans
its whole region; `--count-outlines` measures file-level recall instead.

The threshold is not cosmetic. Bare overlap (`--min-iou 0`) rewards coarse
chunking, since a result spanning a whole class overlaps every symbol in it by
construction. Scored against this repository's pre-hierarchy chunker:

```text
threshold        old chunker        current
overlap > 0      57%  MRR 0.388     50%  MRR 0.375
IoU >= 0.1       17%  MRR 0.124     43%  MRR 0.330
IoU >= 0.3        3%  MRR 0.037     23%  MRR 0.226
IoU >= 0.5        0%  MRR 0.004     17%  MRR 0.159
```

The old chunker leads on exactly one row, the one that scores "the answer is
somewhere in this 4400-line file" as a hit. Quote the threshold with any number
you report — this table is why.

**Cost** — a result *answers* when it carries at least `--min-coverage` of the
answer (default 0.5) or lies inside it, and the cost of that answer is the
total characters of every result up to and including it. That is what the
reader had to get through. `answered@8k` and `answered@32k` are the share of
queries answered inside a budget; the budgets are `--budget`.

Cost exists because IoU measures the wrong thing when methods are short. A
six-line method packed beside its neighbours into one 34-line chunk scores IoU
0.2 and counts as a miss, though the answer is on screen after 34 lines. On
fluentd the median achievable IoU over a 30-method sample is 0.21, so most of
its methods are unscorable at any ranking quality — the number describes Ruby
method length more than it describes retrieval.

The inside-the-answer half of the rule matters just as much. A 19-line `Part`
chunk of a 165-line function covers 12% of it and is the precise pointer;
charging it as a miss would punish exactly the precision the chunk hierarchy
was built for. Requiring coverage alone scored prometheus 0 of 15 while IoU
scored 2.

Cost needs no threshold to reject the coarse chunker that `--min-iou 0` would
reward: a chunk spanning a 4400-line file does contain the answer, and it is
priced accordingly. The budget it is compared against is a context window,
which is a real quantity rather than a tuned constant.

The two disagree on purpose. Report both.

## Reading a result

Two numbers, and they behave differently.

**recall@5** — did the answer land in the top five. Coarse but stable: it moves
in whole queries, and on a 30-query set one query is 3 percentage points.

**MRR@10** — mean reciprocal rank, sensitive to movement that recall cannot
see. It also swings more between variants that differ only by noise.

**answered@budget** — did the answer arrive inside a reading budget. Moves in
whole queries like recall, but counts a hit IoU rejects for being the wrong
zoom rather than the wrong place.

**median cost** — how much had to be read for a typical answered query. The
number to watch when a change makes chunks coarser: recall can hold steady
while cost climbs, and that is a regression the other three numbers hide.

On a 30-query set, treat a one-query difference as nothing. What carries weight
is the shape of the per-query diff: four queries improving with none regressing
is a real effect, while a net +1 with three up and two down is not. Two changes
were rejected on exactly that basis after their summary numbers looked good.

The set is small enough that a change can be tuned into it. Before adopting a
default, check that the win survives on a plateau — if only one parameter value
helps and its neighbours do not, the parameter is fitted to these thirty
queries rather than to search.

## Adding queries

Pick a symbol, write the query someone would actually type, and check it
resolves:

```bash
tests/eval/run_eval.py --repo ~/src/pgjdbc --queries tests/eval/queries/pgjdbc.tsv --verbose
```

An unresolved entry prints a `!!` line and is excluded from the denominator, so
a typo in the file path shows up as a warning rather than as a lower score. So
does a stale digest, which is what a span looks like once the corpus moves
underneath it.
