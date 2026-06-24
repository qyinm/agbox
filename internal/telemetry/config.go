package telemetry

import (
	"bufio"
	"os"
	"path/filepath"
	"strings"
)

const (
	envPostHogAPIKey = "POSTHOG_API_KEY"
	envPostHogHost   = "POSTHOG_HOST"
	defaultPostHogHost = "https://us.i.posthog.com"
)

type Config struct {
	APIKey string
	Host   string
}

func LoadConfig() Config {
	vars := map[string]string{}
	for _, key := range []string{envPostHogAPIKey, envPostHogHost} {
		if v := strings.TrimSpace(os.Getenv(key)); v != "" {
			vars[key] = v
		}
	}
	if home, err := HomeDir(); err == nil {
		mergeEnvFile(filepath.Join(home, ".env"), vars)
	}
	cfg := Config{
		APIKey: strings.TrimSpace(vars[envPostHogAPIKey]),
		Host:   strings.TrimSpace(vars[envPostHogHost]),
	}
	if cfg.Host == "" {
		cfg.Host = defaultPostHogHost
	}
	return cfg
}

func (c Config) Enabled() bool {
	return c.APIKey != ""
}

func mergeEnvFile(path string, vars map[string]string) {
	f, err := os.Open(path)
	if err != nil {
		return
	}
	defer f.Close()
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		line := strings.TrimSpace(sc.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		key, value, ok := strings.Cut(line, "=")
		if !ok {
			continue
		}
		key = strings.TrimSpace(key)
		if key == "" {
			continue
		}
		if _, set := vars[key]; set {
			continue
		}
		value = strings.TrimSpace(value)
		value = strings.Trim(value, `"'`)
		vars[key] = value
	}
}