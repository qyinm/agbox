package claude

import _ "embed"

// DemoSample is the Claude session fixture used by agbox demo and tests.
//
//go:embed testdata/sample.jsonl
var DemoSample []byte