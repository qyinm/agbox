package session

import (
	"strings"

	"github.com/hippoom/agbox/internal/model"
	"github.com/hippoom/agbox/internal/privacy"
)

const minCorrectionChars = 10

type CorrectionPair struct {
	ActionTurn model.Turn
	UserTurn   model.Turn
	Action     model.Action
	UserText   string
}

var imperativeVerbs = map[string]bool{
	"use": true, "don't": true, "dont": true, "do": true, "stop": true,
	"never": true, "always": true, "prefer": true, "avoid": true, "instead": true,
	"fix": true, "change": true, "switch": true, "run": true, "make": true,
	"add": true, "remove": true, "try": true, "need": true, "must": true,
	"should": true, "can't": true, "cannot": true, "wrong": true, "incorrect": true,
	"actually": true, "rather": true,
}

var correctionMarkers = []string{" not ", " never ", " instead ", " rather ", " no "}

// PairCorrections links agent actions to the immediately following user correction.
func PairCorrections(turns []model.Turn, actions []model.Action, userText map[string]string) []CorrectionPair {
	actionByTurn := make(map[string]model.Action, len(actions))
	for _, action := range actions {
		actionByTurn[action.TurnID] = action
	}

	var pairs []CorrectionPair
	var pendingAction *model.Action
	var pendingActionTurn *model.Turn
	awaitingCorrection := false

	for i := range turns {
		turn := turns[i]
		switch turn.Role {
		case "agent":
			if action, ok := actionByTurn[turn.ID]; ok {
				actionCopy := action
				turnCopy := turn
				pendingAction = &actionCopy
				pendingActionTurn = &turnCopy
				awaitingCorrection = true
			}
		case "user":
			if !awaitingCorrection || pendingAction == nil || pendingActionTurn == nil {
				continue
			}
			text, ok := userText[turn.ID]
			if !ok {
				awaitingCorrection = false
				continue
			}
			if looksCorrective(text, *pendingAction) {
				pairs = append(pairs, CorrectionPair{
					ActionTurn: *pendingActionTurn,
					UserTurn:   turn,
					Action:     *pendingAction,
					UserText:   text,
				})
			}
			awaitingCorrection = false
		}
	}

	return pairs
}

func looksCorrective(text string, action model.Action) bool {
	text = strings.TrimSpace(text)
	if len(text) < minCorrectionChars {
		return false
	}

	normalized := privacy.NormalizeSignal(text)
	if normalized == "" {
		return false
	}

	actionNorm := privacy.NormalizeSignal(action.Command)
	if actionNorm == "" {
		actionNorm = privacy.NormalizeSignal(action.Excerpt)
	}
	if actionNorm != "" && normalized == actionNorm {
		return false
	}

	tokens := strings.Fields(normalized)
	if len(tokens) == 0 {
		return false
	}
	if imperativeVerbs[tokens[0]] {
		return true
	}
	for _, token := range tokens {
		if imperativeVerbs[token] {
			return true
		}
	}
	padded := " " + normalized + " "
	for _, marker := range correctionMarkers {
		if strings.Contains(padded, marker) {
			return true
		}
	}
	return false
}