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

| Scenario        | Rust `gotmpl` | Go `text/template` | Go allocs    | Speedup |
| --------------- | ------------- | ------------------ | ------------ | ------- |
| `parse/simple`  | 546 ns        | 1.07 µs            | 31 / 3.0 KiB | 1.95×   |
| `parse/complex` | 2.15 µs       | 3.19 µs            | 69 / 4.6 KiB | 1.48×   |

Rust now wins parse on both cases. The lexer scans the source as bytes
(rather than materializing a `Vec<char>` upfront), tokens borrow their
value directly from the source via `Cow<'a, str>`, and numeric literals
are parsed to `i64` / `f64` at parse time so the executor reads them
with zero conversion. `Template::new` clones a shared `Arc<BTreeMap>`
of builtins (one atomic op) instead of rebuilding the 19-entry map per
call.

### Execute

| Scenario                | Rust `gotmpl` | Go `text/template` | Go allocs      | Speedup |
| ----------------------- | ------------- | ------------------ | -------------- | ------- |
| `exec/simple`           | 125.8 ns      | 147.3 ns           | 4 / 160 B      | 1.17×   |
| `exec/printf`           | 372.7 ns      | 618.5 ns           | 14 / 456 B     | 1.66×   |
| `exec/range_100`        | 3.42 µs       | 9.09 µs            | 103 / 960 B    | 2.66×   |
| `exec/complex_50_users` | 9.09 µs       | 22.46 µs           | 455 / 12.0 KiB | 2.47×   |

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
