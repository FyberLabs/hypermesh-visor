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

	got = runHypermeshConfig(t, dir, "mcp", "bindings", "add", "memory", "--wm-class", "FixtureApp")
	if got.Code != 0 {
		t.Fatalf("bindings add: %s", got.Stderr)
	}
	got = runHypermeshConfig(t, dir, "mcp", "bindings", "ls")
	if got.Code != 0 {
		t.Fatalf("bindings ls: %s", got.Stderr)
	}
	if !strings.Contains(got.Stdout, "memory") || !strings.Contains(got.Stdout, "FixtureApp") {
		t.Fatalf("bindings ls stdout=%q", got.Stdout)
	}
	got = runHypermeshConfig(t, dir, "mcp", "bindings", "ls", "--json")
	if got.Code != 0 {
		t.Fatalf("bindings ls --json: %s", got.Stderr)
	}
	var bindings mcp.BindingsFile
	if err := json.Unmarshal([]byte(got.Stdout), &bindings); err != nil {
		t.Fatal(err)
	}
	if len(bindings.Bindings) != 1 || bindings.Bindings[0].Server != "memory" {
		t.Fatalf("bindings=%+v", bindings)
	}

	projectDir := filepath.Join(dir, "proj", ".hypermesh")
	if err := os.MkdirAll(projectDir, 0o700); err != nil {
		t.Fatal(err)
	}
	projectPath := filepath.Join(projectDir, "mcp.json")
	if err := mcp.WriteCursorSample(projectPath, map[string]mcp.Server{
		"project-tool": {Command: "true"},
	}); err != nil {
		t.Fatal(err)
	}
	got = runHypermeshConfig(t, dir, "mcp", "import", "project", filepath.Join(dir, "proj"))
	if got.Code != 0 {
		t.Fatalf("import project: %s", got.Stderr)
	}
	if !strings.Contains(got.Stdout, "project-tool") {
		t.Fatalf("import project stdout=%q", got.Stdout)
	}

	got = runHypermeshConfig(t, dir, "mcp", "profile", "gateway", "on")
	if got.Code != 0 {
		t.Fatalf("gateway on: %s", got.Stderr)
	}
	got = runHypermeshConfig(t, dir, "mcp", "profile", "add", "chrome")
	if got.Code != 0 {
		t.Fatalf("profile add chrome: %s", got.Stderr)
	}
	got = runHypermeshConfig(t, dir, "mcp", "bindings", "ls")
	if got.Code != 0 {
		t.Fatalf("bindings after chrome: %s", got.Stderr)
	}
	if !strings.Contains(got.Stdout, "chrome") {
		t.Fatalf("expected chrome bindings, got %q", got.Stdout)
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
