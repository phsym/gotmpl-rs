// go_crosscheck is a helper binary for the go-template-rs integration tests.
// It reads a JSON payload from stdin containing a Go template string and typed
// data, executes the template using Go's text/template, and prints the result
// to stdout. Errors go to stderr with a non-zero exit code.
package main

import (
	"encoding/json"
	"fmt"
	"os"
	"strings"
	"text/template"
)

// TypedValue carries a Value with an explicit type tag so that the Rust side
// can preserve the int/float/string/bool distinction across JSON.
type TypedValue struct {
	Type  string                  `json:"type"`
	Value json.RawMessage         `json:"value,omitempty"`
	Items []TypedValue            `json:"items,omitempty"`
	Map   map[string]TypedValue   `json:"map,omitempty"`
}

type Input struct {
	Template string     `json:"template"`
	Data     TypedValue `json:"data"`
}

func decode(tv TypedValue) (interface{}, error) {
	switch tv.Type {
	case "nil":
		return nil, nil
	case "bool":
		var b bool
		if err := json.Unmarshal(tv.Value, &b); err != nil {
			return nil, fmt.Errorf("decode bool: %w", err)
		}
		return b, nil
	case "int":
		var n int64
		if err := json.Unmarshal(tv.Value, &n); err != nil {
			return nil, fmt.Errorf("decode int: %w", err)
		}
		return int(n), nil
	case "uint":
		var n uint64
		if err := json.Unmarshal(tv.Value, &n); err != nil {
			return nil, fmt.Errorf("decode uint: %w", err)
		}
		return uint(n), nil
	case "float":
		var f float64
		if err := json.Unmarshal(tv.Value, &f); err != nil {
			return nil, fmt.Errorf("decode float: %w", err)
		}
		return f, nil
	case "string":
		var s string
		if err := json.Unmarshal(tv.Value, &s); err != nil {
			return nil, fmt.Errorf("decode string: %w", err)
		}
		return s, nil
	case "list":
		result := make([]interface{}, len(tv.Items))
		for i, item := range tv.Items {
			v, err := decode(item)
			if err != nil {
				return nil, fmt.Errorf("decode list[%d]: %w", i, err)
			}
			result[i] = v
		}
		return result, nil
	case "map":
		result := make(map[string]interface{})
		for k, v := range tv.Map {
			decoded, err := decode(v)
			if err != nil {
				return nil, fmt.Errorf("decode map[%q]: %w", k, err)
			}
			result[k] = decoded
		}
		return result, nil
	default:
		return nil, fmt.Errorf("unknown type tag: %q", tv.Type)
	}
}

// funcMap mirrors the custom functions registered in the Rust test harness.
var funcMap = template.FuncMap{
	"add": func(args ...int) (int, error) {
		sum := 0
		for _, a := range args {
			sum += a
		}
		return sum, nil
	},
	"echo": func(arg interface{}) interface{} {
		return arg
	},
	"oneArg": func(s string) (string, error) {
		return fmt.Sprintf("oneArg=%s", s), nil
	},
	"twoArgs": func(a, b string) (string, error) {
		return fmt.Sprintf("twoArgs=%s%s", a, b), nil
	},
	"zeroArgs": func() string {
		return "zeroArgs"
	},
	"count": func(n int) []string {
		chars := "abcdefghijklmnop"
		result := make([]string, n)
		for i := 0; i < n; i++ {
			result[i] = string(chars[i])
		}
		return result
	},
	"makemap": func(args ...string) map[string]string {
		result := make(map[string]string)
		for i := 0; i+1 < len(args); i += 2 {
			result[args[i]] = args[i+1]
		}
		return result
	},
	"mapOfThree": func() map[string]int {
		return map[string]int{"three": 3}
	},
}

func main() {
	var input Input
	if err := json.NewDecoder(os.Stdin).Decode(&input); err != nil {
		fmt.Fprintf(os.Stderr, "json decode: %v\n", err)
		os.Exit(1)
	}

	data, err := decode(input.Data)
	if err != nil {
		fmt.Fprintf(os.Stderr, "data decode: %v\n", err)
		os.Exit(1)
	}

	tmpl, err := template.New("test").Funcs(funcMap).Parse(input.Template)
	if err != nil {
		fmt.Fprintf(os.Stderr, "parse: %v\n", err)
		os.Exit(2)
	}

	var buf strings.Builder
	if err := tmpl.Execute(&buf, data); err != nil {
		fmt.Fprintf(os.Stderr, "exec: %v\n", err)
		os.Exit(2)
	}

	fmt.Print(buf.String())
}
