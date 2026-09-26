package mcp

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

// ImportCursor merges ~/.cursor/mcp.json or .cursor/mcp.json into the store.
func (s Store) ImportCursor(paths ...string) (added []string, err error) {
	candidates := paths
	if len(candidates) == 0 {
		candidates = cursorPaths()
	}
	var lastErr error
	for _, path := range candidates {
		raw, err := os.ReadFile(path)
		if err != nil {
			lastErr = err
			continue
		}
		file, err := decodeFile(raw)
		if err != nil {
			return nil, fmt.Errorf("parse %s: %w", path, err)
		}
		return s.mergeServers(file.Servers)
	}
	if lastErr != nil {
		return nil, fmt.Errorf("no Cursor mcp.json found (tried home and cwd): %w", lastErr)
	}
	return nil, fmt.Errorf("no Cursor mcp.json found")
}

func cursorPaths() []string {
	var out []string
	if cwd, err := os.Getwd(); err == nil {
		out = append(out, filepath.Join(cwd, ".cursor", "mcp.json"))
	}
	if home, err := os.UserHomeDir(); err == nil {
		out = append(out, filepath.Join(home, ".cursor", "mcp.json"))
	}
	return out
}

// ImportProject merges a project-local mcp.json into the store.
// Paths may be a file, a project root (looks for .hypermesh/mcp.json then .cursor/mcp.json),
// or empty (walk cwd parents the same way).
func (s Store) ImportProject(paths ...string) (added []string, err error) {
	candidates := paths
	if len(candidates) == 0 {
		candidates = projectPaths()
	}
	var lastErr error
	for _, path := range candidates {
		filePath, err := resolveProjectMCP(path)
		if err != nil {
			lastErr = err
			continue
		}
		raw, err := os.ReadFile(filePath)
		if err != nil {
			lastErr = err
			continue
		}
		file, err := decodeFile(raw)
		if err != nil {
			return nil, fmt.Errorf("parse %s: %w", filePath, err)
		}
		return s.mergeServers(file.Servers)
	}
	if lastErr != nil {
		return nil, fmt.Errorf("no project mcp.json found: %w", lastErr)
	}
	return nil, fmt.Errorf("no project mcp.json found")
}

func projectPaths() []string {
	var out []string
	if cwd, err := os.Getwd(); err == nil {
		dir := cwd
		for i := 0; i < 8; i++ {
			out = append(out,
				filepath.Join(dir, ".hypermesh", "mcp.json"),
				filepath.Join(dir, ".cursor", "mcp.json"),
			)
			parent := filepath.Dir(dir)
			if parent == dir {
				break
			}
			dir = parent
		}
	}
	return out
}

func resolveProjectMCP(path string) (string, error) {
	path = strings.TrimSpace(path)
	if path == "" {
		return "", fmt.Errorf("empty path")
	}
	st, err := os.Stat(path)
	if err != nil {
		return "", err
	}
	if !st.IsDir() {
		return path, nil
	}
	for _, rel := range []string{
		filepath.Join(".hypermesh", "mcp.json"),
		filepath.Join(".cursor", "mcp.json"),
		"mcp.json",
	} {
		candidate := filepath.Join(path, rel)
		if _, err := os.Stat(candidate); err == nil {
			return candidate, nil
		}
	}
	return "", fmt.Errorf("%s has no .hypermesh/mcp.json or .cursor/mcp.json", path)
}

// ImportDocker ensures a docker-gateway server and adds it to the active profile.
// It prefers an existing `docker mcp gateway run` entry from Cursor-style configs
// when the user already wired Docker that way; otherwise it uses the catalog entry.
func (s Store) ImportDocker(profileName string) (string, error) {
	if err := s.Ensure(); err != nil {
		return "", err
	}
	servers, err := s.LoadServers()
	if err != nil {
		return "", err
	}
	id := "docker-gateway"
	if entry, ok := CatalogByID(id); ok {
		servers.Servers[id] = entry.Server
	} else {
		servers.Servers[id] = Server{
			Type:    "stdio",
			Command: "docker",
			Args:    []string{"mcp", "gateway", "run"},
		}
	}
	if err := s.SaveServers(servers); err != nil {
		return "", err
	}
	profiles, err := s.LoadProfiles()
	if err != nil {
		return "", err
	}
	name := strings.TrimSpace(profileName)
	if name == "" {
		name = profiles.Active
	}
	profile, ok := profiles.Profiles[name]
	if !ok {
		profile = Profile{Servers: []string{}}
	}
	if !contains(profile.Servers, id) {
		profile.Servers = append(profile.Servers, id)
	}
	profiles.Profiles[name] = profile
	if err := s.SaveProfiles(profiles); err != nil {
		return "", err
	}
	return id, nil
}

func (s Store) mergeServers(incoming map[string]Server) ([]string, error) {
	if err := s.Ensure(); err != nil {
		return nil, err
	}
	file, err := s.LoadServers()
	if err != nil {
		return nil, err
	}
	var added []string
	for name, server := range incoming {
		if err := server.Validate(name); err != nil {
			return nil, err
		}
		if _, exists := file.Servers[name]; !exists {
			added = append(added, name)
		}
		file.Servers[name] = server
	}
	if err := s.SaveServers(file); err != nil {
		return nil, err
	}
	profiles, err := s.LoadProfiles()
	if err != nil {
		return nil, err
	}
	active := profiles.Profiles[profiles.Active]
	for _, name := range added {
		if !contains(active.Servers, name) {
			active.Servers = append(active.Servers, name)
		}
	}
	profiles.Profiles[profiles.Active] = active
	if err := s.SaveProfiles(profiles); err != nil {
		return nil, err
	}
	return sortedKeys(mapKeys(added)), nil
}

func mapKeys(ids []string) map[string]struct{} {
	out := map[string]struct{}{}
	for _, id := range ids {
		out[id] = struct{}{}
	}
	return out
}

func contains(list []string, want string) bool {
	for _, item := range list {
		if item == want {
			return true
		}
	}
	return false
}

// ParseCursorRaw is for tests.
func ParseCursorRaw(raw []byte) (File, error) {
	return decodeFile(raw)
}

// WriteCursorSample writes a tiny Cursor-shaped file (tests / docs).
func WriteCursorSample(path string, servers map[string]Server) error {
	raw, err := json.MarshalIndent(File{Servers: servers}, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(path, append(raw, '\n'), 0o600)
}
