package mcp

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

const (
	ServersFileName  = "mcp.json"
	ProfilesFileName = "mcp-profiles.json"
)

type Store struct {
	Dir string
}

func NewStore(dir string) Store {
	return Store{Dir: dir}
}

func (s Store) ServersPath() string {
	return filepath.Join(s.Dir, ServersFileName)
}

func (s Store) ProfilesPath() string {
	return filepath.Join(s.Dir, ProfilesFileName)
}

func (s Store) LoadServers() (File, error) {
	raw, err := os.ReadFile(s.ServersPath())
	if err != nil {
		if os.IsNotExist(err) {
			return File{Servers: map[string]Server{}}, nil
		}
		return File{}, err
	}
	return decodeFile(raw)
}

func (s Store) SaveServers(f File) error {
	if f.Servers == nil {
		f.Servers = map[string]Server{}
	}
	for name, server := range f.Servers {
		if err := server.Validate(name); err != nil {
			return err
		}
	}
	return s.writeJSON(s.ServersPath(), f)
}

func (s Store) LoadProfiles() (ProfilesFile, error) {
	raw, err := os.ReadFile(s.ProfilesPath())
	if err != nil {
		if os.IsNotExist(err) {
			return emptyProfiles(), nil
		}
		return ProfilesFile{}, err
	}
	return decodeProfiles(raw)
}

func (s Store) SaveProfiles(p ProfilesFile) error {
	if p.Profiles == nil {
		p.Profiles = map[string]Profile{}
	}
	if strings.TrimSpace(p.Active) == "" {
		p.Active = "default"
	}
	if _, ok := p.Profiles[p.Active]; !ok {
		p.Profiles[p.Active] = Profile{Servers: []string{}}
	}
	return s.writeJSON(s.ProfilesPath(), p)
}

func (s Store) Ensure() error {
	if err := os.MkdirAll(s.Dir, 0o700); err != nil {
		return err
	}
	if _, err := os.Stat(s.ServersPath()); os.IsNotExist(err) {
		if err := s.SaveServers(File{Servers: map[string]Server{}}); err != nil {
			return err
		}
	}
	if _, err := os.Stat(s.ProfilesPath()); os.IsNotExist(err) {
		if err := s.SaveProfiles(emptyProfiles()); err != nil {
			return err
		}
	}
	return nil
}

func (s Store) writeJSON(path string, v any) error {
	if err := os.MkdirAll(s.Dir, 0o700); err != nil {
		return err
	}
	raw, err := json.MarshalIndent(v, "", "  ")
	if err != nil {
		return err
	}
	raw = append(raw, '\n')
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, raw, 0o600); err != nil {
		return err
	}
	return os.Rename(tmp, path)
}

// ActiveServers returns servers enabled in the active profile (or named profile).
func (s Store) ActiveServers(profileName string) (string, map[string]Server, error) {
	servers, err := s.LoadServers()
	if err != nil {
		return "", nil, err
	}
	profiles, err := s.LoadProfiles()
	if err != nil {
		return "", nil, err
	}
	name := strings.TrimSpace(profileName)
	if name == "" {
		name = profiles.Active
	}
	profile, ok := profiles.Profiles[name]
	if !ok {
		return "", nil, fmt.Errorf("profile %q not found", name)
	}
	out := map[string]Server{}
	if profile.Gateway {
		id := "docker-gateway"
		server, ok := servers.Servers[id]
		if !ok {
			if entry, found := CatalogByID(id); found {
				server = entry.Server
			} else {
				server = Server{
					Type:    "stdio",
					Command: "docker",
					Args:    []string{"mcp", "gateway", "run"},
				}
			}
		}
		if server.IsEnabled() {
			out[id] = applyConfig(server, profile.Config[id])
		}
		return name, out, nil
	}
	for _, id := range profile.Servers {
		server, ok := servers.Servers[id]
		if !ok {
			return "", nil, fmt.Errorf("profile %q names unknown server %q", name, id)
		}
		if !server.IsEnabled() {
			continue
		}
		out[id] = applyConfig(server, profile.Config[id])
	}
	return name, out, nil
}

func applyConfig(server Server, cfg map[string]string) Server {
	if len(cfg) == 0 {
		return server
	}
	if v, ok := cfg["command"]; ok && v != "" {
		server.Command = v
	}
	if v, ok := cfg["url"]; ok && v != "" {
		server.URL = v
	}
	if v, ok := cfg["cwd"]; ok {
		server.Cwd = v
	}
	if v, ok := cfg["type"]; ok && v != "" {
		server.Type = v
	}
	if v, ok := cfg["args"]; ok && v != "" {
		server.Args = splitArgs(v)
	}
	for k, v := range cfg {
		if strings.HasPrefix(k, "env.") {
			if server.Env == nil {
				server.Env = map[string]string{}
			}
			server.Env[strings.TrimPrefix(k, "env.")] = v
		}
		if strings.HasPrefix(k, "headers.") {
			if server.Headers == nil {
				server.Headers = map[string]string{}
			}
			server.Headers[strings.TrimPrefix(k, "headers.")] = v
		}
	}
	return server
}

func splitArgs(v string) []string {
	fields := strings.Fields(v)
	out := make([]string, 0, len(fields))
	out = append(out, fields...)
	return out
}

func sortedKeys[T any](m map[string]T) []string {
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	return keys
}

// SortedServerNames returns server ids in mcp.json order for display.
func SortedServerNames(f File) []string {
	return sortedKeys(f.Servers)
}

// SortedProfileNames returns profile names for display.
func SortedProfileNames(p ProfilesFile) []string {
	return sortedKeys(p.Profiles)
}

// ContainsServer reports whether id is in the profile server list.
func ContainsServer(list []string, id string) bool {
	return contains(list, id)
}
