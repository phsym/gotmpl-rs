# gotmpl benchmarks

Side-by-side numbers for this crate vs Go's
[`text/template`](https://pkg.go.dev/text/template). The Rust benchmarks live
in [benches/template.rs](benches/template.rs) and the Go ones in
[go/template_test.go](go/template_test.go). Both suites use the same templates
and input data, so the numbers line up 1:1.

## Running the benchmarks

### Rust (criterion)

From the workspace root:

```sh
cargo bench -p gotmpl-benches
```

Criterion dumps HTML reports under `target/criterion/` and prints the
three-point estimate (lower, median, upper) for each case.

### Go (`testing.B`)

The Go benchmarks are a standalone module. From `benches/go/`:

```sh
go test -bench=. -benchmem -count=5 -benchtime=3s ./benches/go/template_test.go
```

`-benchmem` adds allocation counts. `-count=5` runs each case five times so you
can eyeball the variance.

## Results

Apple M3, macOS 24.6 (`darwin/arm64`), `rustc 1.94.1`, `go 1.26.1`. All timings
are ns/op — lower is better. Rust figures are the criterion median; Go figures
are the median of five `-count=5` runs.

### Parse

| Scenario        | Rust `gotmpl` | Go `text/template` | Go allocs    | Speedup |
| --------------- | ------------- | ------------------ | ------------ | ------- |
| `parse/simple`  | 509 ns        | 1.07 µs            | 31 / 3.0 KiB | 2.11×   |
| `parse/complex` | 1.96 µs       | 3.21 µs            | 69 / 4.6 KiB | 1.64×   |

### Execute

| Scenario                | Rust `gotmpl` | Go `text/template` | Go allocs      | Speedup |
| ----------------------- | ------------- | ------------------ | -------------- | ------- |
| `exec/simple`           | 89.8 ns       | 147.2 ns           | 4 / 160 B      | 1.64×   |
| `exec/printf`           | 364.8 ns      | 618.9 ns           | 14 / 456 B     | 1.70×   |
| `exec/range_100`        | 3.36 µs       | 9.30 µs            | 103 / 960 B    | 2.77×   |
| `exec/complex_50_users` | 9.84 µs       | 22.46 µs           | 455 / 12.0 KiB | 2.28×   |

The gap opens up fast once there's iteration or any real data to walk. Go pays
for reflection on every field access; here we dispatch directly on the `Value`
enum.

## Methodology

- Same template sources and input shapes in both suites.
- Rust writes into a reused `String` via `execute_fmt`; Go writes into a reused
  `bytes.Buffer`. Both reset between iterations so the allocation numbers
  reflect per-call cost.
- `black_box` keeps LLVM from hoisting inputs out of the Rust parse loops.
- Wall-clock, single-threaded, on AC power with nothing else heavy running.
