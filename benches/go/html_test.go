// Go benchmarks mirroring benches/benches/html.rs, the html/template
// context-aware auto-escaping counterpart of template_test.go.
//
// There are two costs to measure:
//   - BenchmarkEscape* creates a fresh template every iteration, so the escaping
//     analysis (run lazily on the first Execute) is included.
//   - BenchmarkExec* reuses an already-escaped template and measures the render
//     on its own.
package gotmplbench

import (
	"bytes"
	"fmt"
	"html/template"
	"testing"
)

const htmlSrcText = "<p>{{.}}</p>"
const htmlSrcAttr = `<div class="{{.}}"></div>`
const htmlSrcURL = `<a href="{{.}}">x</a>`
const htmlSrcJS = `<script>var x = "{{.}}";</script>`

const htmlSrcPage = `<!DOCTYPE html>
<html lang="en">
<head><title>{{.Title}}</title></head>
<body>
<h1 class="{{.HeadingClass}}">{{.Title}}</h1>
<nav><a href="{{.HomeURL}}">Home</a></nav>
<ul class="posts">
{{- range .Posts}}
  <li data-id="{{.ID}}">
    <a href="{{.URL}}" title="{{.Title}}">{{.Title}}</a>
    <span class="author">{{.Author}}</span>
    <p>{{.Body}}</p>
  </li>
{{- end}}
</ul>
<footer>{{.Footer}}</footer>
<script>var pageId = "{{.PageID}}", count = {{.Count}};</script>
</body>
</html>`

// spicy is a value with characters that need escaping in every context.
func spicy() any { return `a & b < c > "d" 'e' /f/` }

func dataPage() any {
	posts := make([]any, 20)
	for i := range 20 {
		posts[i] = map[string]any{
			"ID":     int64(i),
			"URL":    fmt.Sprintf("/posts/%d?ref=home&sort=new", i),
			"Title":  fmt.Sprintf("Post #%d: \"tips & tricks\"", i),
			"Author": fmt.Sprintf("user%d <team & co>", i),
			"Body": fmt.Sprintf(
				"A short body for post %d. Uses <em>markup</em> & \"quotes\" "+
					"that must be escaped in HTML text.", i),
		}
	}
	return map[string]any{
		"Title":        "My Blog & Notes",
		"HeadingClass": "main headline",
		"HomeURL":      "/home?tab=recent&lang=en",
		"Posts":        posts,
		"Footer":       template.HTML("<small>&copy; 2026 <b>Example</b></small>"),
		"PageID":       "abc-123",
		"Count":        int64(20),
	}
}

// benchHTMLEscape builds a fresh template and executes it once per iteration, so
// the escaping analysis is included in the measurement.
func benchHTMLEscape(b *testing.B, src string, data any) {
	b.Helper()
	var buf bytes.Buffer
	b.ReportAllocs()
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		buf.Reset()
		tmpl := template.Must(template.New("page").Parse(src))
		if err := tmpl.Execute(&buf, data); err != nil {
			b.Fatal(err)
		}
	}
}

// benchHTMLExec reuses an already-escaped template and measures the render alone.
func benchHTMLExec(b *testing.B, src string, data any) {
	b.Helper()
	tmpl := template.Must(template.New("").Parse(src))
	var buf bytes.Buffer
	// Prime the escaper so the analysis is done before the timed loop starts.
	if err := tmpl.Execute(&buf, data); err != nil {
		b.Fatal(err)
	}
	b.ReportAllocs()
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		buf.Reset()
		if err := tmpl.Execute(&buf, data); err != nil {
			b.Fatal(err)
		}
	}
}

func BenchmarkEscapePage(b *testing.B) { benchHTMLEscape(b, htmlSrcPage, dataPage()) }

func BenchmarkExecText(b *testing.B) { benchHTMLExec(b, htmlSrcText, spicy()) }

func BenchmarkExecAttr(b *testing.B) { benchHTMLExec(b, htmlSrcAttr, spicy()) }

func BenchmarkExecURL(b *testing.B) { benchHTMLExec(b, htmlSrcURL, spicy()) }

func BenchmarkExecJS(b *testing.B) { benchHTMLExec(b, htmlSrcJS, spicy()) }

func BenchmarkExecPage20Posts(b *testing.B) { benchHTMLExec(b, htmlSrcPage, dataPage()) }
