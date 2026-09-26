package cli

import (
	"fmt"
	"strings"

	"github.com/spf13/cobra"

	"github.com/FyberLabs/hypermesh-cli/internal/mcp"
)

func newMCPCmd(r *run) *cobra.Command {
	cmd := &cobra.Command{
		Use:   "mcp",
		Short: "Local MCP servers, profiles, and doctor",
		Long:  "Cursor-shaped mcp.json plus Docker-like profiles under the Hypermesh config dir. The visor attaches the active profile when a session opens.",
	}
	cmd.AddCommand(newMCPCatalogCmd(r))
	cmd.AddCommand(newMCPProfileCmd(r))
	cmd.AddCommand(newMCPImportCmd(r))
	cmd.AddCommand(newMCPDoctorCmd(r))
	cmd.AddCommand(newMCPListCmd(r))
	return cmd
}

func (r *run) mcpStore() mcp.Store {
	return mcp.NewStore(r.cfg.Dir)
}

func newMCPCatalogCmd(r *run) *cobra.Command {
	cmd := &cobra.Command{
		Use:   "catalog",
		Short: "Built-in MCP catalog",
	}
	cmd.AddCommand(&cobra.Command{
		Use:   "ls",
		Short: "List catalog servers",
		RunE: func(cmd *cobra.Command, args []string) error {
			entries := mcp.BuiltinCatalog()
			if r.json {
				return r.printJSON(entries)
			}
			for _, entry := range entries {
				fmt.Fprintf(cmd.OutOrStdout(), "%s\t%s\n", entry.ID, entry.Title)
			}
			return nil
		},
	})
	return cmd
}

func newMCPListCmd(r *run) *cobra.Command {
	return &cobra.Command{
		Use:   "list",
		Short: "List configured servers in mcp.json",
		RunE: func(cmd *cobra.Command, args []string) error {
			store := r.mcpStore()
			file, err := store.LoadServers()
			if err != nil {
				return err
			}
			if r.json {
				return r.printJSON(file)
			}
			if len(file.Servers) == 0 {
				fmt.Fprintln(cmd.OutOrStdout(), "(no servers — try: hypermesh mcp import cursor)")
				return nil
			}
			for _, name := range mcp.SortedServerNames(file) {
				server := file.Servers[name]
				fmt.Fprintf(cmd.OutOrStdout(), "%s\t%s\t%s\n", name, server.Transport(), summarizeServer(server))
			}
			return nil
		},
	}
}

func newMCPProfileCmd(r *run) *cobra.Command {
	cmd := &cobra.Command{
		Use:   "profile",
		Short: "Named MCP profiles (Docker-like toolboxes)",
	}
	cmd.AddCommand(&cobra.Command{
		Use:   "ls",
		Short: "List profiles",
		RunE: func(cmd *cobra.Command, args []string) error {
			store := r.mcpStore()
			if err := store.Ensure(); err != nil {
				return err
			}
			profiles, err := store.LoadProfiles()
			if err != nil {
				return err
			}
			if r.json {
				return r.printJSON(profiles)
			}
			for _, name := range mcp.SortedProfileNames(profiles) {
				mark := " "
				if name == profiles.Active {
					mark = "*"
				}
				p := profiles.Profiles[name]
				fmt.Fprintf(cmd.OutOrStdout(), "%s %s\t%d servers\n", mark, name, len(p.Servers))
			}
			return nil
		},
	})
	cmd.AddCommand(&cobra.Command{
		Use:   "create [name]",
		Short: "Create an empty profile",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			store := r.mcpStore()
			if err := store.Ensure(); err != nil {
				return err
			}
			profiles, err := store.LoadProfiles()
			if err != nil {
				return err
			}
			name := args[0]
			if _, exists := profiles.Profiles[name]; exists {
				return fmt.Errorf("profile %q already exists", name)
			}
			profiles.Profiles[name] = mcp.Profile{Servers: []string{}}
			if err := store.SaveProfiles(profiles); err != nil {
				return err
			}
			if r.json {
				return r.printJSON(map[string]any{"ok": true, "profile": name})
			}
			fmt.Fprintf(cmd.OutOrStdout(), "created profile %s\n", name)
			return nil
		},
	})
	cmd.AddCommand(&cobra.Command{
		Use:   "use [name]",
		Short: "Set the active profile",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			store := r.mcpStore()
			if err := store.Ensure(); err != nil {
				return err
			}
			profiles, err := store.LoadProfiles()
			if err != nil {
				return err
			}
			name := args[0]
			if _, ok := profiles.Profiles[name]; !ok {
				return fmt.Errorf("profile %q not found", name)
			}
			profiles.Active = name
			if err := store.SaveProfiles(profiles); err != nil {
				return err
			}
			if r.json {
				return r.printJSON(map[string]any{"ok": true, "active": name})
			}
			fmt.Fprintf(cmd.OutOrStdout(), "active profile %s\n", name)
			return nil
		},
	})
	cmd.AddCommand(&cobra.Command{
		Use:   "add [server]",
		Short: "Add a configured or catalog server to a profile",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			profileName, _ := cmd.Flags().GetString("profile")
			store := r.mcpStore()
			if err := store.Ensure(); err != nil {
				return err
			}
			serverID := args[0]
			servers, err := store.LoadServers()
			if err != nil {
				return err
			}
			if _, ok := servers.Servers[serverID]; !ok {
				entry, ok := mcp.CatalogByID(serverID)
				if !ok {
					return fmt.Errorf("unknown server %q (configure it in mcp.json or use a catalog id)", serverID)
				}
				servers.Servers[serverID] = entry.Server
				if err := store.SaveServers(servers); err != nil {
					return err
				}
			}
			profiles, err := store.LoadProfiles()
			if err != nil {
				return err
			}
			name := profileName
			if name == "" {
				name = profiles.Active
			}
			profile, ok := profiles.Profiles[name]
			if !ok {
				return fmt.Errorf("profile %q not found", name)
			}
			if !mcp.ContainsServer(profile.Servers, serverID) {
				profile.Servers = append(profile.Servers, serverID)
			}
			profiles.Profiles[name] = profile
			if err := store.SaveProfiles(profiles); err != nil {
				return err
			}
			if r.json {
				return r.printJSON(map[string]any{"ok": true, "profile": name, "server": serverID})
			}
			fmt.Fprintf(cmd.OutOrStdout(), "added %s to profile %s\n", serverID, name)
			return nil
		},
	})
	add := cmd.Commands()[len(cmd.Commands())-1]
	add.Flags().String("profile", "", "profile name (default: active)")

	cmd.AddCommand(&cobra.Command{
		Use:   "config",
		Short: "Set or show per-server profile config",
	})
	configCmd := cmd.Commands()[len(cmd.Commands())-1]
	configCmd.AddCommand(&cobra.Command{
		Use:   "set [server.key=value]",
		Short: "Set profile config, e.g. filesystem.cwd=/tmp",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			profileName, _ := cmd.Flags().GetString("profile")
			spec := args[0]
			serverKey, value, ok := strings.Cut(spec, "=")
			if !ok {
				return fmt.Errorf("expected server.key=value")
			}
			serverID, key, ok := strings.Cut(serverKey, ".")
			if !ok || serverID == "" || key == "" {
				return fmt.Errorf("expected server.key=value")
			}
			store := r.mcpStore()
			if err := store.Ensure(); err != nil {
				return err
			}
			profiles, err := store.LoadProfiles()
			if err != nil {
				return err
			}
			name := profileName
			if name == "" {
				name = profiles.Active
			}
			profile, ok := profiles.Profiles[name]
			if !ok {
				return fmt.Errorf("profile %q not found", name)
			}
			if profile.Config == nil {
				profile.Config = map[string]map[string]string{}
			}
			if profile.Config[serverID] == nil {
				profile.Config[serverID] = map[string]string{}
			}
			profile.Config[serverID][key] = value
			profiles.Profiles[name] = profile
			if err := store.SaveProfiles(profiles); err != nil {
				return err
			}
			if r.json {
				return r.printJSON(map[string]any{"ok": true, "profile": name, "server": serverID, "key": key, "value": value})
			}
			fmt.Fprintf(cmd.OutOrStdout(), "%s.%s=%s (profile %s)\n", serverID, key, value, name)
			return nil
		},
	})
	configCmd.PersistentFlags().String("profile", "", "profile name (default: active)")
	return cmd
}

func newMCPImportCmd(r *run) *cobra.Command {
	cmd := &cobra.Command{
		Use:   "import",
		Short: "Import Cursor mcp.json or Docker MCP gateway",
	}
	cmd.AddCommand(&cobra.Command{
		Use:   "cursor [path]",
		Short: "Merge a Cursor mcp.json into Hypermesh config",
		Args:  cobra.MaximumNArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			store := r.mcpStore()
			var added []string
			var err error
			if len(args) == 1 {
				added, err = store.ImportCursor(args[0])
			} else {
				added, err = store.ImportCursor()
			}
			if err != nil {
				return err
			}
			if r.json {
				return r.printJSON(map[string]any{"ok": true, "added": added, "config_dir": r.cfg.Dir})
			}
			if len(added) == 0 {
				fmt.Fprintln(cmd.OutOrStdout(), "imported; no new servers")
				return nil
			}
			fmt.Fprintf(cmd.OutOrStdout(), "imported %s\n", strings.Join(added, ", "))
			return nil
		},
	})
	cmd.AddCommand(&cobra.Command{
		Use:   "docker",
		Short: "Add Docker MCP gateway to the active profile",
		RunE: func(cmd *cobra.Command, args []string) error {
			store := r.mcpStore()
			id, err := store.ImportDocker("")
			if err != nil {
				return err
			}
			if r.json {
				return r.printJSON(map[string]any{"ok": true, "server": id, "config_dir": r.cfg.Dir})
			}
			fmt.Fprintf(cmd.OutOrStdout(), "added %s to active profile\n", id)
			return nil
		},
	})
	return cmd
}

func newMCPDoctorCmd(r *run) *cobra.Command {
	var profile string
	cmd := &cobra.Command{
		Use:   "doctor",
		Short: "Probe servers in a profile (PATH / reachability)",
		RunE: func(cmd *cobra.Command, args []string) error {
			store := r.mcpStore()
			if err := store.Ensure(); err != nil {
				return err
			}
			name, checks, err := store.Doctor(profile)
			if err != nil {
				return err
			}
			if r.json {
				return r.printJSON(map[string]any{"profile": name, "checks": checks})
			}
			fmt.Fprintf(cmd.OutOrStdout(), "profile %s\n", name)
			failed := 0
			for _, check := range checks {
				status := "ok"
				if !check.OK {
					status = "FAIL"
					failed++
				}
				fmt.Fprintf(cmd.OutOrStdout(), "%s\t%s\t%s\t%s\n", status, check.Name, check.Transport, check.Detail)
			}
			if len(checks) == 0 {
				fmt.Fprintln(cmd.OutOrStdout(), "(no servers in profile)")
			}
			if failed > 0 {
				return fmt.Errorf("%d server(s) failed doctor", failed)
			}
			return nil
		},
	}
	cmd.Flags().StringVar(&profile, "profile", "", "profile name (default: active)")
	return cmd
}

func summarizeServer(server mcp.Server) string {
	switch server.Transport() {
	case "stdio":
		parts := []string{server.Command}
		parts = append(parts, server.Args...)
		return strings.Join(parts, " ")
	default:
		return server.URL
	}
}
