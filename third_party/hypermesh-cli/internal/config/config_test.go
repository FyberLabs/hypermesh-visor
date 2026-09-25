package config

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/FyberLabs/hypermesh-cli/internal/api"
)

func TestLoadDefaults(t *testing.T) {
	t.Setenv(EnvAPIBase, "")
	t.Setenv(EnvChatBase, "")
	t.Setenv(EnvAPIKey, "")
	t.Setenv(EnvTenantID, "")
	t.Setenv(EnvRenterUserID, "")
	t.Setenv(EnvLeaseID, "")
	t.Setenv(EnvSuccessURL, "")
	t.Setenv(EnvCancelURL, "")
	dir := t.TempDir()
	cfg, err := LoadFrom(dir)
	if err != nil {
		t.Fatal(err)
	}
	if cfg.APIBase != api.DefaultAPIBase || cfg.ChatBase != api.DefaultChatBase {
		t.Fatalf("%+v", cfg)
	}
}

func TestLoadFileAndEnvOverride(t *testing.T) {
	dir := t.TempDir()
	toml := []byte("api_base = \"https://api.example.test\"\nchat_base = \"https://chat.example.test\"\ntenant_id = \"file-tenant\"\nrenter_user_id = \"file-user\"\n")
	if err := os.WriteFile(ConfigPath(dir), toml, 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(CredentialsPath(dir), []byte("api_key = \"file_key\"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	t.Setenv(EnvAPIBase, "")
	t.Setenv(EnvChatBase, "")
	t.Setenv(EnvAPIKey, "")
	t.Setenv(EnvTenantID, "")
	t.Setenv(EnvRenterUserID, "")
	cfg, err := LoadFrom(dir)
	if err != nil {
		t.Fatal(err)
	}
	if cfg.APIBase != "https://api.example.test" || cfg.APIKey != "" || cfg.TenantID != "file-tenant" {
		t.Fatalf("file load %+v", cfg)
	}
	t.Setenv(EnvAPIBase, "https://api.override.test")
	t.Setenv(EnvAPIKey, "env_key")
	cfg, err = LoadFrom(dir)
	if err != nil {
		t.Fatal(err)
	}
	if cfg.APIBase != "https://api.override.test" || cfg.APIKey != "env_key" {
		t.Fatalf("env override %+v", cfg)
	}
}

func TestWriteProfileDoesNotCreateACredentialFile(t *testing.T) {
	dir := t.TempDir()
	cfg := Config{Dir: dir, APIBase: api.DefaultAPIBase, ChatBase: api.DefaultChatBase}
	if err := os.WriteFile(CredentialsPath(dir), []byte("api_key = \"leftover\"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := cfg.WriteProfile("ten-1", "user-1"); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(CredentialsPath(dir)); !os.IsNotExist(err) {
		t.Fatalf("credentials still present: %v", err)
	}
	loaded, err := LoadFrom(dir)
	if err != nil {
		t.Fatal(err)
	}
	if loaded.APIKey != "" || loaded.TenantID != "ten-1" || loaded.RenterUserID != "user-1" {
		t.Fatalf("%+v", loaded)
	}
	raw, err := os.ReadFile(ConfigPath(dir))
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(raw), "leftover") || strings.Contains(string(raw), "refresh") {
		t.Fatalf("profile leaked a secret: %s", raw)
	}
}

func TestLogoutRemovesCredentials(t *testing.T) {
	dir := t.TempDir()
	cfg := Config{Dir: dir}
	if err := os.WriteFile(CredentialsPath(dir), []byte("api_key = \"org_k\"\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := cfg.Logout(); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(CredentialsPath(dir)); !os.IsNotExist(err) {
		t.Fatalf("credentials still present: %v", err)
	}
}

func TestParseTOML(t *testing.T) {
	t.Parallel()
	m, err := parseTOML([]byte("# c\napi_base = \"https://x\"\ntenant_id = 'abc'\n"))
	if err != nil {
		t.Fatal(err)
	}
	if m["api_base"] != "https://x" || m["tenant_id"] != "abc" {
		t.Fatalf("%v", m)
	}
}

func TestDirUsesHYPERMESH_CONFIG_DIR(t *testing.T) {
	want := filepath.Join(t.TempDir(), "hm")
	t.Setenv(EnvConfigDir, want)
	if Dir() != want {
		t.Fatalf("Dir()=%s", Dir())
	}
}
