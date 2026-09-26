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
