// Go benchmarks mirroring benches/benches/template.rs.
package gotmplbench

import (
	"bytes"
	"fmt"
	"testing"
	"text/template"
)

const srcSimple = "Hello, {{.Name}}!"
const srcPrintf = `{{printf "%s is %d years old" (.Name) (.Age)}}`
const srcRange = "{{range .}}{{.}},{{end}}"
const srcComplex = `{{- range .Users -}}
{{- if .Active }}{{printf "%s <%s> (%d pts)" (.Name) (.Email) (.Score)}}
{{ end -}}
{{- end -}}`

func dataSimple() any {
	return map[string]any{"Name": "World"}
}

func dataPrintf() any {
	return map[string]any{"Name": "Alice", "Age": int64(30)}
}

func dataRange() any {
	xs := make([]int64, 100)
	for i := range xs {
		xs[i] = int64(i)
	}
	return xs
}

func dataComplex() any {
	users := make([]any, 50)
	for i := range 50 {
		users[i] = map[string]any{
			"Name":   fmt.Sprintf("user%d", i),
			"Email":  fmt.Sprintf("user%d@example.com", i),
			"Score":  int64(i * 7),
			"Active": i%2 == 0,
		}
	}
	return map[string]any{"Users": users}
}

func BenchmarkParseSimple(b *testing.B) {
	b.ReportAllocs()
	for i := 0; i < b.N; i++ {
		_, err := template.New("").Parse(srcSimple)
		if err != nil {
			b.Fatal(err)
		}
	}
}

func BenchmarkParseComplex(b *testing.B) {
	b.ReportAllocs()
	for i := 0; i < b.N; i++ {
		_, err := template.New("").Parse(srcComplex)
		if err != nil {
			b.Fatal(err)
		}
	}
}

func benchExec(b *testing.B, src string, data any) {
	b.Helper()
	tmpl := template.Must(template.New("").Parse(src))
	var buf bytes.Buffer
	b.ReportAllocs()
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		buf.Reset()
		if err := tmpl.Execute(&buf, data); err != nil {
			b.Fatal(err)
		}
	}
}

func BenchmarkExecSimple(b *testing.B) { benchExec(b, srcSimple, dataSimple()) }

func BenchmarkExecPrintf(b *testing.B) { benchExec(b, srcPrintf, dataPrintf()) }

func BenchmarkExecRange100(b *testing.B) { benchExec(b, srcRange, dataRange()) }

func BenchmarkExecComplex50Users(b *testing.B) { benchExec(b, srcComplex, dataComplex()) }
