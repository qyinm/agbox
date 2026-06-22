package jsonl

import (
	"crypto/sha256"
	"encoding/hex"
	"strings"

	"github.com/hippoom/agbox/internal/privacy"
)

func StableID(prefix string, parts ...string) string {
	return stableID(prefix, parts...)
}

func stableID(prefix string, parts ...string) string {
	sum := sha256.Sum256([]byte(strings.Join(parts, "|")))
	return prefix + hex.EncodeToString(sum[:])[:16]
}

func hashSignal(s string) string {
	return privacy.HashSignal(s)
}

func normalize(s string) string {
	return privacy.NormalizeSignal(s)
}

func excerpt(s string, n int) string {
	return privacy.Excerpt(s, n)
}

func HashBytes(data []byte) string {
	sum := sha256.Sum256(data)
	return hex.EncodeToString(sum[:])
}