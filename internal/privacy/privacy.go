package privacy

import (
	"crypto/sha256"
	"encoding/hex"
	"regexp"
	"strings"
	"unicode"
)

const MaxSignalBytes = 64 * 1024

var (
	emailRE = regexp.MustCompile(`[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}`)
	tokenRE = regexp.MustCompile(`(?i)(api[_-]?key|token|secret|password)\s*[:=]\s*["']?[^"'\s]+`)
)

func NormalizeSignal(s string) string {
	s = strings.TrimSpace(s)
	var b strings.Builder
	lastSpace := false
	for _, r := range strings.ToLower(s) {
		switch {
		case unicode.IsLetter(r), unicode.IsDigit(r):
			b.WriteRune(r)
			lastSpace = false
		case unicode.IsSpace(r), r == '-' || r == '_' || r == '/' || r == '.':
			if !lastSpace {
				b.WriteByte(' ')
				lastSpace = true
			}
		}
	}
	return strings.TrimSpace(b.String())
}

func HashSignal(normalized string) string {
	sum := sha256.Sum256([]byte(normalized))
	return hex.EncodeToString(sum[:])
}

func Redact(s string) string {
	s = emailRE.ReplaceAllString(s, "[redacted-email]")
	s = tokenRE.ReplaceAllString(s, "$1=[redacted]")
	return strings.TrimSpace(s)
}

func Excerpt(s string, limit int) string {
	s = Redact(strings.TrimSpace(s))
	if limit <= 0 || len(s) <= limit {
		return s
	}
	if limit <= 3 {
		return s[:limit]
	}
	return strings.TrimSpace(s[:limit-3]) + "..."
}

func Slug(s string) string {
	n := NormalizeSignal(s)
	var b strings.Builder
	lastDash := false
	for _, r := range n {
		if unicode.IsLetter(r) || unicode.IsDigit(r) {
			b.WriteRune(r)
			lastDash = false
			continue
		}
		if !lastDash {
			b.WriteByte('-')
			lastDash = true
		}
	}
	out := strings.Trim(b.String(), "-")
	if len(out) > 48 {
		out = strings.Trim(out[:48], "-")
	}
	if out == "" {
		return "workflow-skill"
	}
	return out
}
