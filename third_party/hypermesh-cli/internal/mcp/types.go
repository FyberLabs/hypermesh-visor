package mcp

import (
	"encoding/json"
	"fmt"
	"strings"
)

// File is the Cursor-compatible servers document at mcp.json.
type File struct {
	Servers map[string]Server `json:"mcpServers"`
}

// Server is one MCP server entry (stdio or HTTP/SSE).
type Server struct {
	Type    string            `json:"type,omitempty"`
	Command string            `json:"command,omitempty"`
	Args    []string          `json:"args,omitempty"`
	Env     map[string]string `json:"env,omitempty"`
	URL     string            `json:"url,omitempty"`
	Headers map[string]string `json:"headers,omitempty"`
	Cwd     string            `json:"cwd,omitempty"`
	Enabled *bool             `json:"enabled,omitempty"`
}

func (s Server) Transport() string {
	t := strings.ToLower(strings.TrimSpace(s.Type))
	if t != "" {
		return t
	}
	if strings.TrimSpace(s.URL) != "" {
		return "http"
	}
	return "stdio"
}

func (s Server) IsEnabled() bool {
	if s.Enabled == nil {
		return true
	}
	return *s.Enabled
}

func (s Server) Validate(name string) error {
	name = strings.TrimSpace(name)
	if name == "" {
		return fmt.Errorf("server name is required")
	}
	switch s.Transport() {
	case "stdio":
		if strings.TrimSpace(s.Command) == "" {
			return fmt.Errorf("server %q: stdio requires command", name)
		}
	case "http", "sse":
		if strings.TrimSpace(s.URL) == "" {
			return fmt.Errorf("server %q: %s requires url", name, s.Transport())
		}
	default:
		return fmt.Errorf("server %q: unknown type %q", name, s.Transport())
	}
	return nil
}

// ProfilesFile holds named profiles and the active profile name.
type ProfilesFile struct {
	Active   string             `json:"active"`
	Profiles map[string]Profile `json:"profiles"`
}

// Profile is a named enabled set plus optional per-server config overrides.
type Profile struct {
	Servers []string                     `json:"servers"`
	Config  map[string]map[string]string `json:"config,omitempty"`
	// Gateway asks the visor to attach only the Docker MCP gateway process.
	Gateway bool `json:"gateway,omitempty"`
}

// Matchers map focused apps to this catalog server (written into mcp-bindings.json).
type Matchers struct {
	WMClass    []string `json:"wm_class,omitempty"`
	AppID      []string `json:"app_id,omitempty"`
	Executable []string `json:"executable,omitempty"`
}

// CatalogEntry is a known server the CLI can add without inventing a command.
type CatalogEntry struct {
	ID          string   `json:"id"`
	Title       string   `json:"title"`
	Description string   `json:"description"`
	Server      Server   `json:"server"`
	Matchers    Matchers `json:"matchers,omitempty"`
}

func decodeFile(raw []byte) (File, error) {
	var f File
	if len(strings.TrimSpace(string(raw))) == 0 {
		return File{Servers: map[string]Server{}}, nil
	}
	if err := json.Unmarshal(raw, &f); err != nil {
		return File{}, err
	}
	if f.Servers == nil {
		f.Servers = map[string]Server{}
	}
	return f, nil
}

func decodeProfiles(raw []byte) (ProfilesFile, error) {
	var p ProfilesFile
	if len(strings.TrimSpace(string(raw))) == 0 {
		return emptyProfiles(), nil
	}
	if err := json.Unmarshal(raw, &p); err != nil {
		return ProfilesFile{}, err
	}
	if p.Profiles == nil {
		p.Profiles = map[string]Profile{}
	}
	if strings.TrimSpace(p.Active) == "" {
		p.Active = "default"
	}
	if _, ok := p.Profiles[p.Active]; !ok {
		p.Profiles[p.Active] = Profile{Servers: []string{}}
	}
	return p, nil
}

func emptyProfiles() ProfilesFile {
	return ProfilesFile{
		Active: "default",
		Profiles: map[string]Profile{
			"default": {Servers: []string{}},
		},
	}
}

func boolPtr(v bool) *bool { return &v }
