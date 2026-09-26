package mcp

import (
	"os"
	"path/filepath"
	"testing"
)

func TestSaveLoadAndActiveProfile(t *testing.T) {
	dir := t.TempDir()
	store := NewStore(dir)
	if err := store.Ensure(); err != nil {
		t.Fatal(err)
	}
	file := File{Servers: map[string]Server{
		"filesystem": {
			Type:    "stdio",
			Command: "npx",
			Args:    []string{"-y", "@modelcontextprotocol/server-filesystem", "."},
		},
		"remote": {Type: "http", URL: "http://127.0.0.1:9/mcp"},
	}}
	if err := store.SaveServers(file); err != nil {
		t.Fatal(err)
	}
	profiles := emptyProfiles()
	profiles.Profiles["default"] = Profile{Servers: []string{"filesystem", "remote"}}
	if err := store.SaveProfiles(profiles); err != nil {
		t.Fatal(err)
	}
	name, active, err := store.ActiveServers("")
	if err != nil {
		t.Fatal(err)
	}
	if name != "default" || len(active) != 2 {
		t.Fatalf("active=%q len=%d", name, len(active))
	}
}

func TestImportCursorMergesAndAddsToProfile(t *testing.T) {
	dir := t.TempDir()
	cursor := filepath.Join(dir, ".cursor")
	if err := os.MkdirAll(cursor, 0o700); err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(cursor, "mcp.json")
	if err := WriteCursorSample(path, map[string]Server{
		"memory": {Command: "npx", Args: []string{"-y", "@modelcontextprotocol/server-memory"}},
	}); err != nil {
		t.Fatal(err)
	}
	store := NewStore(filepath.Join(dir, "hypermesh"))
	added, err := store.ImportCursor(path)
	if err != nil {
		t.Fatal(err)
	}
	if len(added) != 1 || added[0] != "memory" {
		t.Fatalf("added=%v", added)
	}
	profiles, err := store.LoadProfiles()
	if err != nil {
		t.Fatal(err)
	}
	if !contains(profiles.Profiles["default"].Servers, "memory") {
		t.Fatalf("profile missing memory: %+v", profiles)
	}
}

func TestImportDockerAddsGateway(t *testing.T) {
	store := NewStore(t.TempDir())
	id, err := store.ImportDocker("")
	if err != nil {
		t.Fatal(err)
	}
	if id != "docker-gateway" {
		t.Fatalf("id=%s", id)
	}
	servers, err := store.LoadServers()
	if err != nil {
		t.Fatal(err)
	}
	gw := servers.Servers["docker-gateway"]
	if gw.Command != "docker" || len(gw.Args) < 2 || gw.Args[0] != "mcp" {
		t.Fatalf("gateway=%+v", gw)
	}
}

func TestDoctorStdioMissingCommand(t *testing.T) {
	store := NewStore(t.TempDir())
	_ = store.Ensure()
	_ = store.SaveServers(File{Servers: map[string]Server{
		"missing": {Command: "hypermesh-mcp-command-does-not-exist-xyz"},
	}})
	profiles := emptyProfiles()
	profiles.Profiles["default"] = Profile{Servers: []string{"missing"}}
	_ = store.SaveProfiles(profiles)
	_, checks, err := store.Doctor("")
	if err != nil {
		t.Fatal(err)
	}
	if len(checks) != 1 || checks[0].OK {
		t.Fatalf("checks=%+v", checks)
	}
}

func TestProfileConfigOverridesCommand(t *testing.T) {
	store := NewStore(t.TempDir())
	_ = store.Ensure()
	_ = store.SaveServers(File{Servers: map[string]Server{
		"fs": {Command: "npx", Args: []string{"-y", "old"}},
	}})
	profiles := emptyProfiles()
	profiles.Profiles["default"] = Profile{
		Servers: []string{"fs"},
		Config: map[string]map[string]string{
			"fs": {"command": "echo", "args": "hello"},
		},
	}
	_ = store.SaveProfiles(profiles)
	_, active, err := store.ActiveServers("default")
	if err != nil {
		t.Fatal(err)
	}
	if active["fs"].Command != "echo" {
		t.Fatalf("%+v", active["fs"])
	}
}
