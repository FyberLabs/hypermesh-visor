package cli

import (
	"fmt"
	"log"
	"os"

	"github.com/spf13/cobra"

	"github.com/FyberLabs/hypermesh-cli/internal/api"
	"github.com/FyberLabs/hypermesh-cli/internal/config"
	"github.com/FyberLabs/hypermesh-cli/internal/oauth"
	"github.com/FyberLabs/hypermesh-cli/internal/session"
)

// ExitFailure is the only non-zero status this process returns.
// Scripts can rely on it. An HTTP status is never the process status.
const ExitFailure = 1

type run struct {
	json     bool
	apiBase  string
	chatBase string
	cfg      config.Config
	client   *api.Client
}

func New(name string) *cobra.Command {
	r := &run{}
	root := &cobra.Command{
		Use:               name,
		Short:             "Thin Hypermesh CLI — Phase 1 Full Model checkout + router chat",
		Long:              "Hypermesh (Hyperme.sh) renter CLI. Phase 1 is Full Model only: catalog llama-3.1-8b-q4, Stripe test Checkout, then chat on the Fyber router.",
		SilenceUsage:      true,
		SilenceErrors:     true,
		CompletionOptions: cobra.CompletionOptions{DisableDefaultCmd: true},
		PersistentPreRunE: func(cmd *cobra.Command, args []string) error {
			cfg, err := config.Load()
			if err != nil {
				return err
			}
			if r.apiBase != "" {
				cfg.APIBase = r.apiBase
			}
			if r.chatBase != "" {
				cfg.ChatBase = r.chatBase
			}
			r.cfg = cfg
			client := api.NewClient(cfg.APIBase, cfg.ChatBase, cfg.APIKey, cfg.TenantID)
			client.Session = session.KeyringStore{}
			client.OAuth = oauth.Panopticon()
			r.client = client
			return nil
		},
	}
	root.PersistentFlags().BoolVar(&r.json, "json", false, "print machine-readable JSON")
	root.PersistentFlags().StringVar(&r.apiBase, "api-base", "", "override HYPERMESH_API_BASE")
	root.PersistentFlags().StringVar(&r.chatBase, "chat-base", "", "override HYPERMESH_CHAT_BASE")

	root.AddCommand(newLoginCmd(r))
	root.AddCommand(newLogoutCmd(r))
	root.AddCommand(newAuthCmd(r))
	root.AddCommand(newCatalogCmd(r))
	root.AddCommand(newClassesCmd(r))
	root.AddCommand(newHostsCmd(r))
	root.AddCommand(newCheckoutCmd(r))
	root.AddCommand(newLeaseCmd(r))
	root.AddCommand(newChatCmd(r))
	root.AddCommand(newPromptCmd(r))
	root.AddCommand(newCompletionsCmd(r))
	root.AddCommand(newMCPCmd(r))
	return root
}

func Execute(name string) {
	os.Exit(Run(name, os.Args[1:]))
}

// Run executes the CLI and returns the process status.
// Diagnostics go to stderr. Script chat writes model text only to stdout.
func Run(name string, args []string) int {
	log.SetOutput(os.Stderr)
	log.SetFlags(0)
	cmd := New(name)
	cmd.SetArgs(args)
	cmd.SetOut(os.Stdout)
	cmd.SetErr(os.Stderr)
	if err := cmd.Execute(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		return ExitFailure
	}
	return 0
}
