# gotmpl benchmarks

Side-by-side numbers for this crate vs Go's
[`text/template`](https://pkg.go.dev/text/template) and
[`html/template`](https://pkg.go.dev/html/template). The Rust benchmarks live
in [benches/template.rs](benches/template.rs) (text) and
[benches/html.rs](benches/html.rs) (html); the Go ones in
[go/template_test.go](go/template_test.go) and [go/html_test.go](go/html_test.go).
Each Rust/Go pair uses the same templates and input data, so the numbers line up
1:1.

## Running the benchmarks

### Rust (criterion)

From the workspace root:

```sh
cargo bench -p gotmpl-benches
```

Criterion dumps HTML reports under `target/criterion/` and prints the
three-point estimate (lower, median, upper) for each case.

### Go (`testing.B`)

The Go benchmarks have no `go.mod`; run them as a file set from the repo root
(both files are the same package, so pass them together):

```sh
go test -bench=. -benchmem -count=5 -benchtime=3s \
    ./benches/go/template_test.go ./benches/go/html_test.go
```

`-benchmem` adds allocation counts. `-count=5` runs each case five times so you
can eyeball the variance. Drop `html_test.go` (or `template_test.go`) to run
just one suite.

## Results

All timings are ns/op (lower is better). Rust figures are the criterion median;
Go figures are the median of five `-count=5` runs.

### `text/template`

Apple M3, macOS 24.6 (`darwin/arm64`), `rustc 1.94.1`, `go 1.26.1`.

#### Parse

| Scenario        | Rust `gotmpl` | Go `text/template` | Go allocs    | Speedup |
| --------------- | ------------- | ------------------ | ------------ | ------- |
| `parse/simple`  | 509 ns        | 1.07 µs            | 31 / 3.0 KiB | 2.11×   |
| `parse/complex` | 1.96 µs       | 3.21 µs            | 69 / 4.6 KiB | 1.64×   |

#### Execute

| Scenario                | Rust `gotmpl` | Go `text/template` | Go allocs      | Speedup |
| ----------------------- | ------------- | ------------------ | -------------- | ------- |
| `exec/simple`           | 89.8 ns       | 147.2 ns           | 4 / 160 B      | 1.64×   |
| `exec/printf`           | 364.8 ns      | 618.9 ns           | 14 / 456 B     | 1.70×   |
| `exec/range_100`        | 3.36 µs       | 9.30 µs            | 103 / 960 B    | 2.77×   |
| `exec/complex_50_users` | 9.84 µs       | 22.46 µs           | 455 / 12.0 KiB | 2.28×   |

The gap opens up fast once there's iteration or any real data to walk. Go pays
for reflection on every field access; here we dispatch directly on the `Value`
enum.

### `html/template`

Apple M3, macOS 24.6 (`darwin/arm64`), `rustc 1.95.0`, `go 1.26.4`. Both engines
run the context-aware escaping analysis lazily on the first execute and cache the
result, so the two suites measure different things. `html_escape/*` builds a
fresh template every iteration and therefore includes the analysis; `html_exec/*`
reuses an already-escaped template and measures the render on its own. The
single-context cases (`text`, `attr`, `url`, `js`) escape one value that is full
of characters needing escapes; `page` ranges over 20 posts, escaping into HTML
text, attribute, URL, and JS-string contexts, plus one trusted field that
bypasses escaping.

#### Escape analysis (fresh template each iteration)

| Scenario       | Rust `gotmpl` | Go `html/template` | Go allocs       | Speedup |
| -------------- | ------------- | ------------------ | --------------- | ------- |
| `html_escape/page` | 64.3 µs   | 91.0 µs            | 1599 / 56.6 KiB | 1.41×   |

#### Execute (already-escaped template)

| Scenario              | Rust `gotmpl` | Go `html/template` | Go allocs      | Speedup |
| --------------------- | ------------- | ------------------ | -------------- | ------- |
| `html_exec/text`          | 342.6 ns  | 461.6 ns           | 10 / 408 B     | 1.35×   |
| `html_exec/attr`          | 339.7 ns  | 465.1 ns           | 10 / 408 B     | 1.37×   |
| `html_exec/url`           | 769.5 ns  | 1.76 µs            | 19 / 640 B     | 2.28×   |
| `html_exec/js`            | 367.0 ns  | 462.5 ns           | 9 / 376 B      | 1.26×   |
| `html_exec/page_20_posts` | 34.4 µs   | 71.5 µs            | 1299 / 34.9 KiB| 2.08×   |

URL escaping shows the widest single-context gap. Go routes it through
`url.Parse` and reflection, while `gotmpl` normalizes the bytes inline. On the
full page, the iteration and per-context escaping compound the same reflection
cost we saw in the text suite.

## Methodology

- Same template sources and input shapes in both suites.
- Rust writes into a reused `String` via `execute_fmt`; Go writes into a reused
  `bytes.Buffer`. Both reset between iterations so the allocation numbers
  reflect per-call cost.
- `black_box` keeps LLVM from hoisting inputs out of the Rust parse loops.
- For `html/template`, the escaping analysis runs once and is cached.
  `html_escape/*` builds a fresh template per iteration to measure it;
  `html_exec/*` primes the cache before the timed loop so it measures render
  only. The Rust html benches need
  the `html` feature, enabled for the `gotmpl` dependency in
  [Cargo.toml](Cargo.toml).
- Wall-clock, single-threaded, on AC power with nothing else heavy running.
