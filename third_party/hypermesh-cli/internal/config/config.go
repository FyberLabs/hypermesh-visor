package config

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/FyberLabs/hypermesh-cli/internal/api"
)

const (
	EnvAPIBase      = "HYPERMESH_API_BASE"
	EnvChatBase     = "HYPERMESH_CHAT_BASE"
	EnvAPIKey       = "HYPERMESH_API_KEY"
	EnvTenantID     = "HYPERMESH_TENANT_ID"
	EnvRenterUserID = "HYPERMESH_RENTER_USER_ID"
	EnvLeaseID      = "HYPERMESH_LEASE_ID"
	EnvConfigDir    = "HYPERMESH_CONFIG_DIR"
	EnvSuccessURL   = "HYPERMESH_SUCCESS_URL"
	EnvCancelURL    = "HYPERMESH_CANCEL_URL"
)

type Config struct {
	APIBase      string
	ChatBase     string
	APIKey       string
	TenantID     string
	RenterUserID string
	LeaseID      string
	SuccessURL   string
	CancelURL    string
	Dir          string
}

func Dir() string {
	if d := strings.TrimSpace(os.Getenv(EnvConfigDir)); d != "" {
		return d
	}
	if xdg := strings.TrimSpace(os.Getenv("XDG_CONFIG_HOME")); xdg != "" {
		return filepath.Join(xdg, "hypermesh")
	}
	home, err := os.UserHomeDir()
	if err != nil || home == "" {
		return filepath.Join(".", ".config", "hypermesh")
	}
	return filepath.Join(home, ".config", "hypermesh")
}

func ConfigPath(dir string) string {
	return filepath.Join(dir, "config.toml")
}

func CredentialsPath(dir string) string {
	return filepath.Join(dir, "credentials")
}

func Load() (Config, error) {
	return LoadFrom(Dir())
}

func LoadFrom(dir string) (Config, error) {
	cfg := Config{
		APIBase:  api.DefaultAPIBase,
		ChatBase: api.DefaultChatBase,
		Dir:      dir,
	}
	if raw, err := os.ReadFile(ConfigPath(dir)); err == nil {
		m, err := parseTOML(raw)
		if err != nil {
			return Config{}, fmt.Errorf("parse %s: %w", ConfigPath(dir), err)
		}
		applyMap(&cfg, m)
	} else if !os.IsNotExist(err) {
		return Config{}, fmt.Errorf("read %s: %w", ConfigPath(dir), err)
	}
	if raw, err := os.ReadFile(CredentialsPath(dir)); err == nil {
		m, err := parseTOML(raw)
		if err != nil {
			return Config{}, fmt.Errorf("parse %s: %w", CredentialsPath(dir), err)
		}
		if v := m["api_key"]; v != "" {
			cfg.APIKey = v
		}
	} else if !os.IsNotExist(err) {
		return Config{}, fmt.Errorf("read %s: %w", CredentialsPath(dir), err)
	}
	applyEnv(&cfg)
	return cfg, nil
}

func applyMap(cfg *Config, m map[string]string) {
	if v := m["api_base"]; v != "" {
		cfg.APIBase = v
	}
	if v := m["chat_base"]; v != "" {
		cfg.ChatBase = v
	}
	if v := m["tenant_id"]; v != "" {
		cfg.TenantID = v
	}
	if v := m["renter_user_id"]; v != "" {
		cfg.RenterUserID = v
	}
	if v := m["success_url"]; v != "" {
		cfg.SuccessURL = v
	}
	if v := m["cancel_url"]; v != "" {
		cfg.CancelURL = v
	}
}

func applyEnv(cfg *Config) {
	if v := strings.TrimSpace(os.Getenv(EnvAPIBase)); v != "" {
		cfg.APIBase = v
	}
	if v := strings.TrimSpace(os.Getenv(EnvChatBase)); v != "" {
		cfg.ChatBase = v
	}
	if v := strings.TrimSpace(os.Getenv(EnvAPIKey)); v != "" {
		cfg.APIKey = v
	}
	if v := strings.TrimSpace(os.Getenv(EnvTenantID)); v != "" {
		cfg.TenantID = v
	}
	if v := strings.TrimSpace(os.Getenv(EnvRenterUserID)); v != "" {
		cfg.RenterUserID = v
	}
	if v := strings.TrimSpace(os.Getenv(EnvLeaseID)); v != "" {
		cfg.LeaseID = v
	}
	if v := strings.TrimSpace(os.Getenv(EnvSuccessURL)); v != "" {
		cfg.SuccessURL = v
	}
	if v := strings.TrimSpace(os.Getenv(EnvCancelURL)); v != "" {
		cfg.CancelURL = v
	}
}

func (c Config) WriteLogin(apiKey, tenantID, renterUserID string) error {
	if err := api.ValidateRenterKey(apiKey); err != nil {
		return err
	}
	if strings.TrimSpace(tenantID) == "" {
		return fmt.Errorf("tenant id is required")
	}
	if err := os.MkdirAll(c.Dir, 0o700); err != nil {
		return err
	}
	cfgMap := map[string]string{
		"api_base":  first(c.APIBase, api.DefaultAPIBase),
		"chat_base": first(c.ChatBase, api.DefaultChatBase),
		"tenant_id": tenantID,
	}
	if renterUserID != "" {
		cfgMap["renter_user_id"] = renterUserID
	} else if c.RenterUserID != "" {
		cfgMap["renter_user_id"] = c.RenterUserID
	}
	if c.SuccessURL != "" {
		cfgMap["success_url"] = c.SuccessURL
	}
	if c.CancelURL != "" {
		cfgMap["cancel_url"] = c.CancelURL
	}
	if err := os.WriteFile(ConfigPath(c.Dir), encodeTOML(cfgMap), 0o644); err != nil {
		return err
	}
	cred := encodeTOML(map[string]string{"api_key": apiKey})
	if err := os.WriteFile(CredentialsPath(c.Dir), cred, 0o600); err != nil {
		return err
	}
	return os.Chmod(CredentialsPath(c.Dir), 0o600)
}

func (c Config) Logout() error {
	p := CredentialsPath(c.Dir)
	if err := os.Remove(p); err != nil && !os.IsNotExist(err) {
		return err
	}
	return nil
}

func first(values ...string) string {
	for _, v := range values {
		if strings.TrimSpace(v) != "" {
			return v
		}
	}
	return ""
}

func parseTOML(raw []byte) (map[string]string, error) {
	out := map[string]string{}
	for i, line := range strings.Split(string(raw), "\n") {
		line = strings.TrimSpace(line)
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		key, val, ok := strings.Cut(line, "=")
		if !ok {
			return nil, fmt.Errorf("line %d: expected key = value", i+1)
		}
		key = strings.TrimSpace(key)
		val = strings.TrimSpace(val)
		if len(val) >= 2 {
			if (val[0] == '"' && val[len(val)-1] == '"') || (val[0] == '\'' && val[len(val)-1] == '\'') {
				val = val[1 : len(val)-1]
			}
		}
		out[key] = val
	}
	return out, nil
}

func encodeTOML(m map[string]string) []byte {
	order := []string{"api_base", "chat_base", "tenant_id", "renter_user_id", "success_url", "cancel_url", "api_key"}
	var b strings.Builder
	seen := map[string]bool{}
	for _, k := range order {
		if v, ok := m[k]; ok {
			fmt.Fprintf(&b, "%s = %q\n", k, v)
			seen[k] = true
		}
	}
	for k, v := range m {
		if !seen[k] {
			fmt.Fprintf(&b, "%s = %q\n", k, v)
		}
	}
	return []byte(b.String())
}
