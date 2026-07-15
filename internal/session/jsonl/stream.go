package jsonl

import (
	"bufio"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"slices"
	"strconv"
	"strings"
	"time"

	"github.com/hippoom/agbox/internal/model"
)

const (
	ContextStateVersion = 1
	MaxMetadataBytes    = 4 << 10
	MaxRecordBytes      = 64 << 20
	MaxSliceBytes       = 64 << 20
	MaxSliceRecords     = 1024
	MaxJSONDepth        = 64
	MaxJSONTokens       = 200_000
	MaxSliceDuration    = 5 * time.Second
	MaxContextSeedBytes = 256 << 10
)

var (
	ErrMalformedRecord = errors.New("malformed jsonl record")
	ErrSignalTooLarge  = errors.New("correction-relevant value exceeds limit")
	ErrRecordBudget    = errors.New("jsonl record exceeds parser budget")
	ErrParserState     = errors.New("invalid parser continuation state")
	ErrMissingContext  = errors.New("correction context unavailable at baseline")
)

// CapturePaths maps normalized JSON paths to the maximum decoded string size
// required by a native handler. Array indexes are represented by "*".
type CapturePaths map[string]int

type CapturedValue struct {
	Value     string
	Indexes   []int
	Oversized bool
}

// Record contains only explicitly requested, bounded scalar values. It never
// owns an entire source line or an unselected JSON value.
type Record struct {
	Offset int64
	Values map[string][]CapturedValue
}

func (r Record) First(path string) string {
	values := r.Values[path]
	if len(values) == 0 {
		return ""
	}
	return values[0].Value
}

func (r Record) All(path string) []CapturedValue {
	return r.Values[path]
}

func (r Record) At(path string, indexes ...int) string {
	value, _ := r.Captured(path, indexes...)
	return value.Value
}

func (r Record) Captured(path string, indexes ...int) (CapturedValue, bool) {
	for _, value := range r.Values[path] {
		if slices.Equal(value.Indexes, indexes) {
			return value, true
		}
	}
	return CapturedValue{}, false
}

// NativeHandler declares the exact bounded fields needed from its agent's
// schema and converts one complete record into normalized entities.
type NativeHandler interface {
	CapturePaths() CapturePaths
	ProcessRecord(record Record, ctx *Context, acc *Accum, meta Meta) error
}

type StreamResult struct {
	Accum              Accum
	NewOffset          int64
	ParserStateVersion int
	ParserState        []byte
	BytesRead          int64
	Incomplete         bool
}

type persistedContext struct {
	TurnIndex         int    `json:"t"`
	LastActionID      string `json:"a,omitempty"`
	LastActionTurn    string `json:"r,omitempty"`
	RequireAction     bool   `json:"n,omitempty"`
	LastUserSignature string `json:"u,omitempty"`
}

func decodeContext(state []byte) (*Context, error) {
	ctx := &Context{}
	if len(state) == 0 {
		return ctx, nil
	}
	var saved persistedContext
	if err := json.Unmarshal(state, &saved); err != nil || saved.TurnIndex < 0 {
		return nil, ErrParserState
	}
	ctx.TurnIndex = saved.TurnIndex
	ctx.RequireLastAction = saved.RequireAction
	ctx.LastUserSignature = saved.LastUserSignature
	if saved.LastActionID != "" {
		ctx.LastAction = &model.Action{ID: saved.LastActionID, TurnID: saved.LastActionTurn}
	}
	return ctx, nil
}

func encodeContext(ctx *Context) ([]byte, error) {
	saved := persistedContext{TurnIndex: ctx.TurnIndex, RequireAction: ctx.RequireLastAction, LastUserSignature: ctx.LastUserSignature}
	if ctx.LastAction != nil {
		saved.LastActionID = ctx.LastAction.ID
		saved.LastActionTurn = ctx.LastAction.TurnID
	}
	return json.Marshal(saved)
}

// MissingContextState marks a structural EOF baseline that intentionally did
// not replay old history. An immediate correction will fail diagnostically;
// a newly appended action establishes fresh linkage and clears the marker.
func MissingContextState() []byte {
	state, _ := json.Marshal(persistedContext{RequireAction: true})
	return state
}

// ProcessStream seeks directly to startOffset and processes complete records
// from there. A checkpoint already at EOF performs no content Read calls.
func ProcessStream(source io.ReadSeeker, startOffset int64, state []byte, handler NativeHandler, meta Meta) (StreamResult, error) {
	result := StreamResult{NewOffset: startOffset, ParserStateVersion: ContextStateVersion}
	if startOffset < 0 {
		return result, fmt.Errorf("%w: negative checkpoint", ErrParserState)
	}
	end, err := source.Seek(0, io.SeekEnd)
	if err != nil {
		return result, err
	}
	if startOffset > end {
		return result, fmt.Errorf("%w: checkpoint beyond eof", ErrParserState)
	}
	ctx, err := decodeContext(state)
	if err != nil {
		return result, err
	}
	if startOffset == end {
		result.ParserState, _ = encodeContext(ctx)
		return result, nil
	}
	if _, err := source.Seek(startOffset, io.SeekStart); err != nil {
		return result, err
	}

	p := &streamParser{
		r:        bufio.NewReaderSize(source, 32<<10),
		captures: handler.CapturePaths(),
	}
	safeOffset := startOffset
	records := 0
	sliceDeadline := time.Now().Add(MaxSliceDuration)
	for p.total < MaxSliceBytes && records < MaxSliceRecords {
		if records > 0 && time.Now().After(sliceDeadline) {
			break
		}
		recordStart := startOffset + p.total
		p.recordStart = p.total
		p.tokens = 0
		// The slice budget controls yielding between records. Once a record is
		// started it receives its own bounded parse budget so a healthy record is
		// never quarantined merely because earlier records consumed the slice.
		p.deadline = time.Now().Add(MaxSliceDuration)
		record := Record{Offset: recordStart, Values: make(map[string][]CapturedValue)}
		p.record = &record
		err := p.parseValue(nil, nil, 0)
		if errors.Is(err, io.EOF) {
			result.NewOffset = safeOffset
			break
		}
		if errors.Is(err, io.ErrUnexpectedEOF) {
			result.NewOffset = safeOffset
			result.Incomplete = true
			break
		}
		if err != nil {
			result.NewOffset = startOffset
			return result, err
		}
		complete, err := p.consumeRecordTerminator()
		if errors.Is(err, io.ErrUnexpectedEOF) || !complete {
			result.NewOffset = safeOffset
			result.Incomplete = true
			break
		}
		if err != nil {
			result.NewOffset = startOffset
			return result, err
		}
		if err := handler.ProcessRecord(record, ctx, &result.Accum, meta); err != nil {
			result.NewOffset = startOffset
			return result, err
		}
		records++
		safeOffset = startOffset + p.total
		result.NewOffset = safeOffset
		if safeOffset >= end {
			break
		}
	}
	result.BytesRead = p.total
	result.ParserState, err = encodeContext(ctx)
	return result, err
}

type streamParser struct {
	r           *bufio.Reader
	captures    CapturePaths
	record      *Record
	total       int64
	recordStart int64
	tokens      int
	deadline    time.Time
}

func (p *streamParser) readByte() (byte, error) {
	if p.total-p.recordStart >= MaxRecordBytes || p.tokens >= MaxJSONTokens || (p.total&4095 == 0 && time.Now().After(p.deadline)) {
		return 0, ErrRecordBudget
	}
	b, err := p.r.ReadByte()
	if err != nil {
		if errors.Is(err, io.EOF) && p.total > p.recordStart {
			return 0, io.ErrUnexpectedEOF
		}
		return 0, err
	}
	p.total++
	return b, nil
}

func (p *streamParser) unreadByte() error {
	if err := p.r.UnreadByte(); err != nil {
		return err
	}
	p.total--
	return nil
}

func (p *streamParser) nextNonSpace() (byte, error) {
	for {
		b, err := p.readByte()
		if err != nil {
			return 0, err
		}
		switch b {
		case ' ', '\t', '\r':
			continue
		case '\n':
			return 0, ErrMalformedRecord
		default:
			return b, nil
		}
	}
}

func (p *streamParser) parseValue(path, indexes []string, depth int) error {
	if depth > MaxJSONDepth {
		return ErrRecordBudget
	}
	b, err := p.nextNonSpace()
	if err != nil {
		return err
	}
	p.tokens++
	switch b {
	case '{':
		return p.parseObject(path, indexes, depth+1)
	case '[':
		return p.parseArray(path, indexes, depth+1)
	case '"':
		return p.parseCapturedString(path, indexes)
	case 't':
		return p.consumeLiteral("rue")
	case 'f':
		return p.consumeLiteral("alse")
	case 'n':
		return p.consumeLiteral("ull")
	default:
		if b == '-' || b >= '0' && b <= '9' {
			return p.consumeNumber()
		}
		return ErrMalformedRecord
	}
}

func (p *streamParser) parseObject(path, indexes []string, depth int) error {
	b, err := p.nextNonSpace()
	if err != nil {
		return err
	}
	if b == '}' {
		return nil
	}
	if b != '"' {
		return ErrMalformedRecord
	}
	for {
		key, oversized, err := p.readString(MaxMetadataBytes, true)
		if err != nil {
			return err
		}
		if oversized {
			return ErrRecordBudget
		}
		b, err = p.nextNonSpace()
		if err != nil || b != ':' {
			return ErrMalformedRecord
		}
		if err := p.parseValue(appendPath(path, key), indexes, depth); err != nil {
			return err
		}
		b, err = p.nextNonSpace()
		if err != nil {
			return err
		}
		switch b {
		case '}':
			return nil
		case ',':
			b, err = p.nextNonSpace()
			if err != nil || b != '"' {
				return ErrMalformedRecord
			}
		default:
			return ErrMalformedRecord
		}
	}
}

func (p *streamParser) parseArray(path, indexes []string, depth int) error {
	b, err := p.nextNonSpace()
	if err != nil {
		return err
	}
	if b == ']' {
		return nil
	}
	if err := p.unreadByte(); err != nil {
		return err
	}
	for index := 0; ; index++ {
		if err := p.parseValue(appendPath(path, "*"), append(indexes, strconv.Itoa(index)), depth); err != nil {
			return err
		}
		b, err = p.nextNonSpace()
		if err != nil {
			return err
		}
		switch b {
		case ']':
			return nil
		case ',':
			continue
		default:
			return ErrMalformedRecord
		}
	}
}

func (p *streamParser) parseCapturedString(path, indexes []string) error {
	key := strings.Join(path, ".")
	limit, capture := p.captures[key]
	value, oversized, err := p.readString(limit, capture)
	if err != nil {
		return err
	}
	if capture {
		idx := make([]int, len(indexes))
		for i, raw := range indexes {
			idx[i], _ = strconv.Atoi(raw)
		}
		p.record.Values[key] = append(p.record.Values[key], CapturedValue{Value: value, Indexes: idx, Oversized: oversized})
	}
	return nil
}

// readString is called after the opening quote has already been consumed.
func (p *streamParser) readString(limit int, capture bool) (string, bool, error) {
	var raw strings.Builder
	oversized := false
	if capture {
		raw.WriteByte('"')
	}
	for {
		b, err := p.readByte()
		if err != nil {
			return "", false, err
		}
		if b < 0x20 {
			return "", false, ErrMalformedRecord
		}
		if b == '"' {
			if !capture {
				return "", false, nil
			}
			if oversized {
				return "", true, nil
			}
			raw.WriteByte('"')
			decoded, err := strconv.Unquote(raw.String())
			if err != nil {
				return "", false, ErrMalformedRecord
			}
			if len(decoded) > limit {
				return "", true, nil
			}
			return decoded, false, nil
		}
		if b == '\\' {
			if capture && !oversized {
				raw.WriteByte(b)
			}
			escaped, err := p.readByte()
			if err != nil {
				return "", false, err
			}
			if !strings.ContainsRune(`"\\/bfnrtu`, rune(escaped)) {
				return "", false, ErrMalformedRecord
			}
			if capture && !oversized {
				raw.WriteByte(escaped)
			}
			if escaped == 'u' {
				for i := 0; i < 4; i++ {
					h, err := p.readByte()
					if err != nil || !isHex(h) {
						return "", false, ErrMalformedRecord
					}
					if capture && !oversized {
						raw.WriteByte(h)
					}
				}
			}
			if capture && !oversized && raw.Len() > limit*6+2 {
				oversized = true
				raw.Reset()
			}
			continue
		}
		if capture && !oversized {
			raw.WriteByte(b)
			// JSON escaping can expand the raw form, so allow bounded slack but
			// stop retaining hostile values while still validating the record.
			if raw.Len() > limit*6+2 {
				oversized = true
				raw.Reset()
			}
		}
	}
}

func (p *streamParser) consumeLiteral(rest string) error {
	for i := range rest {
		b, err := p.readByte()
		if err != nil {
			return err
		}
		if b != rest[i] {
			return ErrMalformedRecord
		}
	}
	return nil
}

func (p *streamParser) consumeNumber() error {
	for {
		b, err := p.readByte()
		if errors.Is(err, io.ErrUnexpectedEOF) {
			return err
		}
		if err != nil {
			return err
		}
		if (b >= '0' && b <= '9') || strings.ContainsRune("+-.eE", rune(b)) {
			continue
		}
		return p.unreadByte()
	}
}

func (p *streamParser) consumeRecordTerminator() (bool, error) {
	for {
		b, err := p.readByte()
		if err != nil {
			return false, err
		}
		switch b {
		case ' ', '\t', '\r':
			continue
		case '\n':
			return true, nil
		default:
			return false, ErrMalformedRecord
		}
	}
}

func appendPath(path []string, part string) []string {
	out := make([]string, len(path)+1)
	copy(out, path)
	out[len(path)] = part
	return out
}

func isHex(b byte) bool {
	return b >= '0' && b <= '9' || b >= 'a' && b <= 'f' || b >= 'A' && b <= 'F'
}
