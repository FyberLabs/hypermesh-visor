package cli

import (
	"bytes"
	"encoding/json"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"github.com/FyberLabs/hypermesh-cli/internal/mcp"
)

func TestMCPCatalogAndImportDoctor(t *testing.T) {
	dir := t.TempDir()

	got := runHypermeshConfig(t, dir, "mcp", "catalog", "ls")
	if got.Code != 0 {
		t.Fatalf("catalog ls: %s", got.Stderr)
	}
	if !strings.Contains(got.Stdout, "filesystem") || !strings.Contains(got.Stdout, "docker-gateway") {
		t.Fatalf("catalog stdout=%q", got.Stdout)
	}

	cursorDir := filepath.Join(dir, "cursor-home", ".cursor")
	if err := os.MkdirAll(cursorDir, 0o700); err != nil {
		t.Fatal(err)
	}
	cursorPath := filepath.Join(cursorDir, "mcp.json")
	if err := mcp.WriteCursorSample(cursorPath, map[string]mcp.Server{
		"memory": {Command: "true"},
	}); err != nil {
		t.Fatal(err)
	}

	got = runHypermeshConfig(t, dir, "mcp", "import", "cursor", cursorPath)
	if got.Code != 0 {
		t.Fatalf("import: %s", got.Stderr)
	}
	if !strings.Contains(got.Stdout, "memory") {
		t.Fatalf("import stdout=%q", got.Stdout)
	}

	got = runHypermeshConfig(t, dir, "mcp", "profile", "ls", "--json")
	if got.Code != 0 {
		t.Fatalf("profile ls: %s", got.Stderr)
	}
	var profiles mcp.ProfilesFile
	if err := json.Unmarshal([]byte(got.Stdout), &profiles); err != nil {
		t.Fatal(err)
	}
	if profiles.Active != "default" || !mcp.ContainsServer(profiles.Profiles["default"].Servers, "memory") {
		t.Fatalf("profiles=%+v", profiles)
	}

	got = runHypermeshConfig(t, dir, "mcp", "doctor")
	if got.Code != 0 {
		t.Fatalf("doctor: stdout=%q stderr=%q", got.Stdout, got.Stderr)
	}
	if !strings.Contains(got.Stdout, "memory") || !strings.Contains(got.Stdout, "ok") {
		t.Fatalf("doctor stdout=%q", got.Stdout)
	}

	got = runHypermeshConfig(t, dir, "mcp", "import", "docker")
	if got.Code != 0 {
		t.Fatalf("import docker: %s", got.Stderr)
	}
	got = runHypermeshConfig(t, dir, "mcp", "list")
	if got.Code != 0 {
		t.Fatalf("list: %s", got.Stderr)
	}
	if !strings.Contains(got.Stdout, "docker-gateway") {
		t.Fatalf("list stdout=%q", got.Stdout)
	}
}

func runHypermeshConfig(t *testing.T, configDir string, args ...string) scriptResult {
	t.Helper()
	cmd := exec.Command(testBinary(t), args...)
	cmd.Env = append(os.Environ(),
		"HYPERMESH_CONFIG_DIR="+configDir,
		"HYPERMESH_API_KEY=org_key",
		"HYPERMESH_TENANT_ID=ten",
	)
	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr
	err := cmd.Run()
	code := 0
	if err != nil {
		var ee *exec.ExitError
		if !errors.As(err, &ee) {
			t.Fatalf("run: %v\nstderr: %s", err, stderr.String())
		}
		code = ee.ExitCode()
	}
	return scriptResult{Stdout: stdout.String(), Stderr: stderr.String(), Code: code}
}
