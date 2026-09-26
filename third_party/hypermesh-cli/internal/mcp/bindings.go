package mcp

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

const BindingsFileName = "mcp-bindings.json"

// Binding maps a focused app identity to an MCP server id.
type Binding struct {
	Server     string `json:"server"`
	WMClass    string `json:"wm_class,omitempty"`
	AppID      string `json:"app_id,omitempty"`
	Executable string `json:"executable,omitempty"`
}

// BindingsFile is ~/.config/hypermesh/mcp-bindings.json.
type BindingsFile struct {
	Bindings []Binding `json:"bindings"`
}

func (s Store) BindingsPath() string {
	return filepath.Join(s.Dir, BindingsFileName)
}

func (s Store) LoadBindings() (BindingsFile, error) {
	raw, err := os.ReadFile(s.BindingsPath())
	if err != nil {
		if os.IsNotExist(err) {
			return BindingsFile{Bindings: []Binding{}}, nil
		}
		return BindingsFile{}, err
	}
	var file BindingsFile
	if len(strings.TrimSpace(string(raw))) == 0 {
		return BindingsFile{Bindings: []Binding{}}, nil
	}
	if err := json.Unmarshal(raw, &file); err != nil {
		return BindingsFile{}, err
	}
	if file.Bindings == nil {
		file.Bindings = []Binding{}
	}
	return file, nil
}

func (s Store) SaveBindings(file BindingsFile) error {
	if file.Bindings == nil {
		file.Bindings = []Binding{}
	}
	for i, b := range file.Bindings {
		if strings.TrimSpace(b.Server) == "" {
			return fmt.Errorf("binding %d: server is required", i)
		}
		if strings.TrimSpace(b.WMClass) == "" && strings.TrimSpace(b.AppID) == "" && strings.TrimSpace(b.Executable) == "" {
			return fmt.Errorf("binding %d: need wm_class, app_id, or executable", i)
		}
	}
	return s.writeJSON(s.BindingsPath(), file)
}

// AddBinding appends or replaces a binding for the same server + matcher key.
func (s Store) AddBinding(b Binding) error {
	if err := s.Ensure(); err != nil {
		return err
	}
	file, err := s.LoadBindings()
	if err != nil {
		return err
	}
	replaced := false
	for i := range file.Bindings {
		if file.Bindings[i].Server == b.Server &&
			file.Bindings[i].WMClass == b.WMClass &&
			file.Bindings[i].AppID == b.AppID &&
			file.Bindings[i].Executable == b.Executable {
			file.Bindings[i] = b
			replaced = true
			break
		}
	}
	if !replaced {
		file.Bindings = append(file.Bindings, b)
	}
	return s.SaveBindings(file)
}
