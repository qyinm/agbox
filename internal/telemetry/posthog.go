package telemetry

import (
	"bytes"
	"context"
	"crypto/tls"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
	"time"
)

const captureTimeout = 5 * time.Second

type captureRequest struct {
	APIKey     string         `json:"api_key"`
	Event      string         `json:"event"`
	DistinctID string         `json:"distinct_id"`
	Properties map[string]any `json:"properties"`
}

type Client struct {
	cfg       Config
	transport http.RoundTripper
	now       func() time.Time
}

func NewClient(cfg Config) *Client {
	return &Client{
		cfg: cfg,
		transport: &http.Transport{
			TLSClientConfig: &tls.Config{MinVersion: tls.VersionTLS12},
		},
		now: time.Now,
	}
}

func (c *Client) Capture(event, distinctID string, props map[string]any) error {
	if !c.cfg.Enabled() || distinctID == "" {
		return nil
	}
	if props == nil {
		props = map[string]any{}
	}
	props["$process_person_profile"] = false

	body, err := json.Marshal(captureRequest{
		APIKey:     c.cfg.APIKey,
		Event:      event,
		DistinctID: distinctID,
		Properties: props,
	})
	if err != nil {
		return err
	}

	captureURL, err := posthogCaptureURL(c.cfg.Host)
	if err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(context.Background(), captureTimeout)
	defer cancel()

	req, err := http.NewRequestWithContext(ctx, http.MethodPost, captureURL, bytes.NewReader(body))
	if err != nil {
		return err
	}
	req.Header.Set("Content-Type", "application/json")

	httpClient := &http.Client{
		Transport: c.transport,
		CheckRedirect: func(req *http.Request, via []*http.Request) error {
			return http.ErrUseLastResponse
		},
	}
	resp, err := httpClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	_, _ = io.Copy(io.Discard, resp.Body)
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return fmt.Errorf("posthog capture: HTTP %d", resp.StatusCode)
	}
	return nil
}

func posthogCaptureURL(host string) (string, error) {
	host = strings.TrimSpace(host)
	if host == "" {
		return "", fmt.Errorf("posthog host is empty")
	}
	u, err := url.Parse(host)
	if err != nil {
		return "", fmt.Errorf("posthog host: %w", err)
	}
	if u.Scheme != "https" || u.Host == "" {
		return "", fmt.Errorf("posthog host must use https")
	}
	if !allowedPostHogHost(u.Hostname()) {
		return "", fmt.Errorf("posthog host not allowlisted: %s", u.Hostname())
	}
	return strings.TrimRight(u.String(), "/") + "/capture/", nil
}

func allowedPostHogHost(host string) bool {
	host = strings.ToLower(strings.TrimSpace(host))
	switch host {
	case "us.i.posthog.com", "eu.i.posthog.com", "app.posthog.com":
		return true
	}
	return strings.HasSuffix(host, ".posthog.com")
}