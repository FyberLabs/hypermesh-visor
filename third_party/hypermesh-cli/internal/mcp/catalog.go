package mcp

// BuiltinCatalog is a small Hypermesh desktop set plus a Docker gateway entry.
// Import can add more; this is what `mcp catalog ls` shows without a network.
func BuiltinCatalog() []CatalogEntry {
	return []CatalogEntry{
		{
			ID:          "filesystem",
			Title:       "Filesystem",
			Description: "Local filesystem tools via @modelcontextprotocol/server-filesystem",
			Server: Server{
				Type:    "stdio",
				Command: "npx",
				Args:    []string{"-y", "@modelcontextprotocol/server-filesystem", "."},
			},
		},
		{
			ID:          "memory",
			Title:       "Memory",
			Description: "Simple memory tools via @modelcontextprotocol/server-memory",
			Server: Server{
				Type:    "stdio",
				Command: "npx",
				Args:    []string{"-y", "@modelcontextprotocol/server-memory"},
			},
		},
		{
			ID:          "docker-gateway",
			Title:       "Docker MCP Gateway",
			Description: "Docker Desktop MCP Toolkit gateway (requires Docker MCP Toolkit)",
			Server: Server{
				Type:    "stdio",
				Command: "docker",
				Args:    []string{"mcp", "gateway", "run"},
			},
		},
		{
			ID:          "chrome",
			Title:       "Chrome / Chromium",
			Description: "Browser automation MCP when installed; matchers for common window classes",
			Server: Server{
				Type:    "stdio",
				Command: "npx",
				Args:    []string{"-y", "@modelcontextprotocol/server-everything"},
			},
			Matchers: Matchers{
				WMClass: []string{"google-chrome", "Google-chrome", "chromium", "Chromium"},
				AppID:   []string{"google-chrome", "chromium"},
			},
		},
		{
			ID:          "code",
			Title:       "VS Code / Cursor",
			Description: "Editor-oriented MCP entry with desktop matchers",
			Server: Server{
				Type:    "stdio",
				Command: "npx",
				Args:    []string{"-y", "@modelcontextprotocol/server-filesystem", "."},
			},
			Matchers: Matchers{
				WMClass: []string{"Code", "code", "cursor", "Cursor"},
				AppID:   []string{"code", "cursor"},
			},
		},
		{
			ID:          "hypermesh-desktop",
			Title:       "Hypermesh desktop (inverse)",
			Description: "Expose visor view/mouse/type as MCP tools via hypermesh-desktop-mcp",
			Server: Server{
				Type:    "stdio",
				Command: "hypermesh-desktop-mcp",
				Args:    []string{},
				Env: map[string]string{
					"HYPERMESH_VISOR_URL":  "${env:HYPERMESH_VISOR_URL}",
					"HYPERMESH_SESSION_ID": "${env:HYPERMESH_SESSION_ID}",
				},
			},
		},
	}
}

func CatalogByID(id string) (CatalogEntry, bool) {
	for _, entry := range BuiltinCatalog() {
		if entry.ID == id {
			return entry, true
		}
	}
	return CatalogEntry{}, false
}

// BindingsFromMatchers expands catalog matchers into Binding rows for a server id.
func BindingsFromMatchers(server string, m Matchers) []Binding {
	var out []Binding
	for _, wm := range m.WMClass {
		if wm == "" {
			continue
		}
		out = append(out, Binding{Server: server, WMClass: wm})
	}
	for _, app := range m.AppID {
		if app == "" {
			continue
		}
		out = append(out, Binding{Server: server, AppID: app})
	}
	for _, exe := range m.Executable {
		if exe == "" {
			continue
		}
		out = append(out, Binding{Server: server, Executable: exe})
	}
	return out
}
