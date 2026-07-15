package jsonl

import (
	"crypto/sha256"
	"encoding/hex"
	"fmt"
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

// CheckpointHash is a bounded progress fingerprint. Source generation owns
// replacement detection, so adapters never re-hash an entire transcript.
func CheckpointHash(sourceID string, offset int64, parserState []byte) string {
	return HashBytes([]byte(fmt.Sprintf("%s|%d|%x", sourceID, offset, sha256.Sum256(parserState))))
}
