- automatic conversion of data to value when passed to execute

- wasm support
- Error for (custom) function returns ?
- func() and funcs() shall accept functions with generic types
- function definition helpers for argument conversion & validation
- Replace recursive walk by iterative walk

- Trait + `Value`'s variant for making native rust values `Value` compatible

- Panic in user-provided funcs shall add backtrace to error, or at least print it on stderr
- impl From<T> for Value for common types (zero copy) and also impl From<&T> where T: ToValue

- add criterion benchmarks. Compare with go benchmarks

- Propagate source `Pos` from AST into exec errors so runtime errors carry line/col
- Migrate `TemplateError::Exec(String)` call sites to structured variants

- C FFI, Python integration, Node.JS integration, WASM integration
- Pin 3rd party github actions using their sha256 ref

- Search for optimization opportunities. Reduce allocations

- Support utf8 in func names, and field names