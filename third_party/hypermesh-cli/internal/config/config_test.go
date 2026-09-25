package config

import (
	"os"
	"path/filepath"
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
	if cfg.APIBase != "https://api.example.test" || cfg.APIKey != "file_key" || cfg.TenantID != "file-tenant" {
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

func TestWriteLoginPermissionsAndRejectHostKey(t *testing.T) {
	dir := t.TempDir()
	cfg := Config{Dir: dir, APIBase: api.DefaultAPIBase, ChatBase: api.DefaultChatBase}
	if err := cfg.WriteLogin("hm_dev_nope", "ten", ""); err == nil {
		t.Fatal("accepted host key")
	}
	if err := cfg.WriteLogin("org_renter_key", "ten-1", "user-1"); err != nil {
		t.Fatal(err)
	}
	st, err := os.Stat(CredentialsPath(dir))
	if err != nil {
		t.Fatal(err)
	}
	if st.Mode().Perm() != 0o600 {
		t.Fatalf("credentials mode %o", st.Mode().Perm())
	}
	loaded, err := LoadFrom(dir)
	if err != nil {
		t.Fatal(err)
	}
	if loaded.APIKey != "org_renter_key" || loaded.TenantID != "ten-1" || loaded.RenterUserID != "user-1" {
		t.Fatalf("%+v", loaded)
	}
	if loaded.APIBase != api.DefaultAPIBase {
		t.Fatalf("api base %s", loaded.APIBase)
	}
}

func TestLogoutRemovesCredentials(t *testing.T) {
	dir := t.TempDir()
	cfg := Config{Dir: dir}
	if err := cfg.WriteLogin("org_k", "t", ""); err != nil {
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
