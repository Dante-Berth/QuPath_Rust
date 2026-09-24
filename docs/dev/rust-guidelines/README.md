# Rust Guidelines — Imaging Engine Overlay

Companion to `rust-guidelines.txt` (Pragmatic Rust Guidelines, MIT, Microsoft).
That document targets networked service libraries. This project is a CPU-bound,
memory-bound whole-slide image processing engine. Where the two conflict, **this
file wins**.

Read this file first. Then read only the sections of `rust-guidelines.txt`
listed under "In force" below.

---

## 0. Prime directive

Rust is not the source of the speedup. Rust-vs-Java on equivalent code is
1–3×. The speedup comes from, in descending order of impact:

1. **Doing less work** — correct pyramid level, correct ROI, lazy feature
   computation, early rejection.
2. **Data layout** — columnar, contiguous, cache-resident.
3. **Parallel granularity** — tile-level, not pixel-level.
4. **Avoiding allocation and copies** in the inner loop.
5. **SIMD** — only after 1–4 are done.

Any change justified by "Rust is faster" and not by one of the five above is
rejected. Every performance claim must be backed by committed benchmark output.

---

## 1. Not in force (do not apply, do not read)

These sections of `rust-guidelines.txt` do not apply. This engine has no async
runtime, no network surface, and no service topology.

- `M-ASYNC-STACK-SIZE`, `M-YIELD-POINTS`, `M-ASYNC-FN` — no async in this
  project. Parallelism is `rayon`, not `tokio`. Do not introduce an async
  runtime; if you believe you need one, stop and ask.
- `M-LOG-STRUCTURED` (OTel semantic conventions, message templates) — replaced
  by §7 below.
- `M-ISOLATE-DLL-STATE` — no Rust-to-Rust DLL boundaries.
- `M-SERVICES-CLONE`, `M-DI-HIERARCHY`, `M-INIT-BUILDER` deps/`fundle` patterns
  — service-shaped DI is not the shape of this codebase.
- `M-MOCKABLE-SYSCALLS` in full generality — see §8 for the narrower rule.
- `M-TYPES-SEND` reasoning about futures — types must still be `Send` for
  `rayon`, but the futures discussion is irrelevant.

## 2. In force, promoted to highest priority

Read these carefully; they are the load-bearing ones here.

- `M-HOTPATH` — profile first, always. Non-negotiable, see §3.
- `M-RUST-SHAPED` — this is a port from Java. This rule is the difference
  between a fast engine and a slow Java program written in Rust syntax.
- `M-AVOID-INDIRECTION`, `M-BOX-DST`, `M-INITIAL-CAPACITY`, `M-MEM-REUSE`,
  `M-SHRINK-TO-FIT`, `M-FAST-HASHER`, `M-THROUGHPUT`
- `M-UNSAFE`, `M-UNSOUND`, `M-UNSAFE-IMPLIES-UB`
- `M-TAUTOLOGICAL-TESTS` — the single most common agent failure mode.
- `M-TARGET-CPU` (target `x86-64-v3` for AVX2; document the floor),
  `M-MIMALLOC`
- `M-STATIC-VERIFICATION` — clippy config applies verbatim.
- `M-FFI-NAMING`, `M-FFI-TRANSLATES` — relevant, we will link OpenSlide.
- Project layout: `M-CARGO-WORKSPACE`, `M-CRATES-FLAT-FOLDER`,
  `M-CRATES-IN-WORKSPACE`, `M-LATEST-EDITION`
- Universal: `M-DOCUMENTED-MAGIC`, `M-SHORT-NAMES`, `M-WEASEL-WORDS`,
  `M-PUBLIC-DEBUG`, `M-LINT-OVERRIDE-EXPECT`
- Correctness: all `M-PANIC-*` items.

---

## 3. Profile before porting (I-PROFILE-FIRST)

No subsystem may be ported before a profile of the equivalent QuPath path
exists. Produce it with async-profiler or JFR against a real slide, commit the
flame graph to `docs/profiles/`, and name in the PR the top three frames by
self time.

Port those frames. Nothing else. If the profile shows time is spent in JPEG or
JPEG2000 decode, the answer is not a Rust port — it is a better codec, a
different pyramid level, or better caching, and that must be stated before any
Rust is written.

Expected honest outcomes, so nobody is surprised:

| Workload | Realistic gain |
| --- | --- |
| Decode-bound tile reading | ~1×, possibly slower |
| Per-pixel filters, thresholding, morphology | 2–5×, up to 15× with SIMD |
| Measurement computation and object storage | 10–100× (layout, §4) |
| Object filtering, spatial queries, export | 10–50× |
| Whole pipeline, end to end | 3–10× is a good result |

A reported gain above 100× on a whole pipeline is a measurement bug until
proven otherwise. Investigate before reporting it.

---

## 4. Objects are columnar (I-COLUMNAR)

This is the highest-value design decision in the project.

QuPath stores each detection as an object holding a string-keyed measurement
map of boxed doubles. Do not reproduce this. Detections and their measurements
are stored as a struct-of-arrays table: one contiguous `Vec<f32>` (or `Vec<f64>`
— see §5) per feature, one row index per object, feature names interned once in
a side table.

```rust
// Bad: transliterated from Java. Pointer chase per measurement, cache-hostile,
// ~200 bytes of overhead per object before any data.
struct Detection {
    roi: Roi,
    measurements: HashMap<String, f64>,
}
type Objects = Vec<Detection>;

// Good: contiguous per-feature columns, O(1) typed access, vectorizable,
// trivially serialized, and filtering touches only the columns it needs.
struct Detections {
    n: usize,
    rois: Vec<Roi>,
    features: Vec<Box<str>>,   // interned names, index == column index
    columns: Vec<Vec<f32>>,    // columns[f][i] == feature f of object i
}
```

Consequences that must be respected:

- Feature access is by resolved column index, never by string lookup in a loop.
  Resolve names to indices once, outside the loop.
- Filtering produces an index list (`Vec<u32>`), never a cloned object set.
- Geometry (`Roi`) stays separate from measurements; most operations touch only
  one of the two.

## 5. Float policy (I-FLOAT-POLICY)

- The float width of every stored measurement is a deliberate, documented
  decision, matched to what QuPath stores. Do not default to `f64` because it
  "feels safer" — it halves your cache throughput and doubles memory on the
  largest structure in the program.
- Exact equality with QuPath will not hold. Java dropped `strictfp` guarantees
  in 17, and Rust's libm differs from Java's `Math` in the last ULP. Tests
  compare with documented tolerances: relative `1e-5` for scalar measurements,
  Dice or IoU `>= 0.99` for masks and segmentations.
- **Parallel float reduction must be deterministic.** Float addition is not
  associative, so a naive `par_iter().sum()` gives a different answer depending
  on how `rayon` split the work — the result changes between runs on the same
  machine. Reductions over measurements must use fixed-size chunks reduced in a
  fixed order, or a compensated (Kahan/Neumaier) accumulator. Silent run-to-run
  variation in reported measurements is a correctness bug, and it is the single
  most common way a parallel port loses reproducibility. Add a test that runs a
  reduction under `RAYON_NUM_THREADS` of 1, 2, and 8 and asserts bit-identical
  results.

## 6. Parallelism and memory (I-PARALLEL, I-STREAMING)

- Parallelize at **tile granularity**. Never `par_iter()` over pixels — the
  work per item is smaller than the scheduling overhead. Never nest `par_iter`.
- A whole-slide image does not fit in RAM. There is no code path that
  materializes a full-resolution slide into a `Vec<u8>`. Everything is a
  streaming tile pipeline with a bounded queue and a documented memory ceiling.
  Add a test that asserts peak RSS stays under the ceiling on a large fixture.
- Reuse tile buffers across iterations (`M-MEM-REUSE`). Allocation in the tile
  loop is a defect.
- Prefer `chunks_exact`, `zip`, and iterator chains over indexed loops. This
  elides bounds checks and enables autovectorization **without any `unsafe`**,
  and it is by a wide margin the cheapest performance win available. Reach for
  `get_unchecked` only when a profile and a disassembly show the bound check is
  actually the cost, and then follow `M-UNSAFE` in full.
- SIMD only after the above, only behind a scalar fallback, and only with a
  differential test asserting the two paths agree.

## 7. Telemetry (I-LOGGING)

Use `tracing` with plain spans. No OpenTelemetry conventions, no message
templates. The inner tile loop emits nothing at all — one span per tile batch,
not per tile, not per object. Progress reporting goes through a callback the
caller owns, not through the log.

## 8. Errors (I-ERRORS)

Image I/O is fallible in ordinary, expected ways: truncated files, unsupported
compression, missing pyramid levels, malformed metadata. These are `Result`, and
they carry the file path and the pyramid level.

Do not `unwrap()` in library code. An agent under time pressure will spray
`.unwrap()` through the decode path; this is grounds for rejecting the change.
`M-PANIC-ON-BUG` still applies for genuine invariant violations.

Follow `M-ERRORS-CANONICAL-STRUCTS` for library crates and `M-APP-ERROR` for the
CLI.

## 9. Native dependencies (I-NATIVE)

JPEG2000 has no mature pure-Rust decoder, and Aperio SVS commonly uses it.
Plan for FFI to OpenSlide and/or OpenJPEG from the start; do not begin writing a
TIFF parser on the assumption that pure Rust will get there.

FFI lives in a `*-sys` crate, wrapped by a safe crate, per `M-FFI-NAMING` and
`M-FFI-TRANSLATES`. `M-OOBE` and `M-SYS-CRATES` apply: vendor the sources,
pin the hash, build via `cc`, never shell out to a Makefile.

**Licensing:** OpenSlide is LGPL and QuPath is GPLv3. If this code is derived
from reading QuPath source, it is a derivative work and must be GPLv3. Decide
this before the first commit; it cannot be undone retroactively.

---

## 10. Rules for agents working in this repo (I-AGENT)

These exist because they are the observed failure modes, not hypothetical ones.

- **Never weaken a test to make it pass.** Do not edit a golden fixture, do not
  widen a tolerance, do not add `#[ignore]`. If parity cannot be reached, stop
  and report it with the numbers.
- **Never claim a speedup without evidence.** Every performance claim carries
  committed `criterion` output, on the same machine, same thread count, same
  fixture, with the JVM warmed and its startup excluded. State explicitly what
  was excluded.
- **No stubs presented as complete.** `todo!()` is acceptable; a function that
  silently returns a plausible wrong value is not.
- **Report divergence from QuPath, don't hide it.** Every intentional behavioural
  difference goes in `DIVERGENCES.md` with the reason. QuPath's embedded ImageJ
  has idiosyncratic boundary handling; the target is matching QuPath, not
  matching the textbook.
- **No `unsafe` without a committed profile** showing what it buys, plus a
  `// SAFETY:` comment and a Miri run.
- Tests asserting constants against themselves are noise (`M-TAUTOLOGICAL-TESTS`).
  Parity tests against recorded QuPath output are the real test suite.
- Work on a branch, one logical change per commit,
  `cargo test && cargo clippy -- -D warnings && cargo fmt --check` green before
  every commit. Never `--no-verify`.
- **Record the lesson, not the change, in `docs/dev/EVOLUTION.md`.** Not every
  commit, and not every N commits — a fixed cadence produces filler on a quiet
  week and misses the one that mattered on a busy one. `git log` already holds
  what changed; that file holds what would change what the next person *tries*.

  Write an entry when a commit produces one of these, and it is usually neither
  the largest nor the hardest commit:

  - **A reverted or abandoned experiment.** Highest value in the file, because
    the code is gone and the only trace is a commit titled after something
    else. Includes anything measured at ~1.0x and dropped. Record it *with the
    number* — an experiment recorded without its measurement gets repeated.
  - **A wrong assumption that a test agreed with.** A fixture built from a
    model of the data will agree with code built from the same model. When the
    real artefact disagreed, that is transferable.
  - **A mistake made in more than one place.** Three commits fixing one shape
    of bug is one entry, written the third time.
  - **An invalidated invariant** — a guard documented as never firing that now
    fires, a comment whose justification a caller change expired.

  Do **not** write an entry for a fix that is only interesting inside one
  function; that belongs in its doc comment. If an entry cannot say what a
  reader should do differently, it is a changelog line and `git log` has it.

  Cite the commit hash and keep the newest first. Adding the entry in the same
  commit as the work is fine and preferred — it is the moment the reasoning is
  still in your head.