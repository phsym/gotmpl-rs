# gotmpl benchmarks

Side-by-side benchmarks comparing this crate (`gotmpl`) against Go's reference
[`text/template`](https://pkg.go.dev/text/template) implementation. The Rust
benchmarks live in [benches/template.rs](benches/template.rs) and the Go
benchmarks in [go/template_test.go](go/template_test.go) — both share the
same templates and input data so the numbers can be compared directly.

## Running the benchmarks

### Rust (criterion)

From the workspace root:

```sh
cargo bench -p gotmpl-benches
```

Criterion writes HTML reports to `target/criterion/` and prints the three-point
estimate (lower bound, median, upper bound) for each case.

### Go (`testing.B`)

The Go benchmarks are a standalone module. From `benches/go/`:

```sh
go test -bench=. -benchmem -count=5 -benchtime=3s ./benches/go/template_test.go
```

`-benchmem` adds allocation counts and `-count=5` runs each benchmark five
times so variance is visible.

## Results

Measured on an Apple M3 (macOS 24.6, `darwin/arm64`) with
`rustc 1.94.1` / `go 1.26.1`. Timings are ns/op (lower is better); Rust
numbers are the criterion median, Go numbers are the median of five
`-count=5` runs.

### Parse

| Scenario        | Rust `gotmpl` | Go `text/template` | Go allocs    |
| --------------- | ------------- | ------------------ | ------------ |
| `parse/simple`  | 4.75 µs       | 1.11 µs            | 31 / 3.0 KiB |
| `parse/complex` | 10.12 µs      | 3.24 µs            | 69 / 4.6 KiB |

Go wins on parse: its lexer/parser is one of the most-tuned pieces of the
standard library and builds up a tree of pointer-linked nodes rather than
the `Arc`-wrapped enums used here.

### Execute

| Scenario                | Rust `gotmpl` | Go `text/template` | Go allocs      | Speedup |
| ----------------------- | ------------- | ------------------ | -------------- | ------- |
| `exec/simple`           | 137.5 ns      | 147.6 ns           | 4 / 160 B      | 1.07×   |
| `exec/printf`           | 577.2 ns      | 625.3 ns           | 14 / 456 B     | 1.08×   |
| `exec/range_100`        | 3.34 µs       | 9.15 µs            | 103 / 960 B    | 2.74×   |
| `exec/complex_50_users` | 18.72 µs      | 22.78 µs           | 455 / 12.0 KiB | 1.22×   |

Execution is where this crate pulls ahead — especially once there is
iteration or non-trivial data to walk. Go's `text/template` pays for
reflection on every field access; `gotmpl` dispatches directly on its
`Value` enum.

## Methodology notes

- Both suites use identical template sources and input shapes (see the
  `SRC_*` / `src*` constants and `data*` helpers).
- Rust benchmarks use `execute_fmt` into a reused `String`; Go benchmarks
  use `Execute` into a reused `bytes.Buffer`. Both reset the buffer each
  iteration so allocation numbers reflect per-call cost.
- `black_box` is used in the Rust parse benchmarks to keep LLVM from
  hoisting the input out of the loop.
- Numbers are wall-clock, single-threaded, on battery-off AC power with
  no other heavy processes running. Don't read too much into sub-10%
  differences.
