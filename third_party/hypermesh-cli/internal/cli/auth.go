package cli

import (
	"fmt"
	"net/http"

	"github.com/spf13/cobra"

	"github.com/FyberLabs/hypermesh-cli/internal/api"
	"github.com/FyberLabs/hypermesh-cli/internal/oauth"
	"github.com/FyberLabs/hypermesh-cli/internal/session"
)

func newAuthCmd(r *run) *cobra.Command {
	cmd := &cobra.Command{
		Use:   "auth",
		Short: "Sign in, show the session, or log out",
	}
	cmd.AddCommand(newAuthLoginCmd(r))
	cmd.AddCommand(newAuthWhoamiCmd(r))
	cmd.AddCommand(newAuthLogoutCmd(r))
	return cmd
}

func newAuthLoginCmd(r *run) *cobra.Command {
	var device bool
	cmd := &cobra.Command{
		Use:   "login",
		Short: "Sign in (same as `hypermesh login`)",
		RunE: func(cmd *cobra.Command, args []string) error {
			return signIn(cmd.OutOrStdout(), cmd.ErrOrStderr(), session.KeyringStore{}, oauth.Panopticon(), http.DefaultClient, device, oauth.DisplayAvailable(), oauth.OpenSystemBrowser, r.cfg.ForgetPlaintextCredentials, r.noteTenant)
		},
	}
	cmd.Flags().BoolVar(&device, "device", false, "print a device code instead of opening a browser")
	return cmd
}

func newAuthWhoamiCmd(r *run) *cobra.Command {
	return &cobra.Command{
		Use:   "whoami",
		Short: "Show local renter identity (no extra IdP call)",
		RunE: func(cmd *cobra.Command, args []string) error {
			keyOK := r.cfg.APIKey != ""
			var keyErr string
			if keyOK {
				if err := api.ValidateRenterKey(r.cfg.APIKey); err != nil {
					keyErr = err.Error()
				}
			}
			refresh, sessionErr := (session.KeyringStore{}).Refresh()
			if sessionErr != nil && !keyOK {
				return sessionErr
			}
			sessionSet := sessionErr == nil && refresh != ""
			out := map[string]any{
				"api_base":       r.cfg.APIBase,
				"chat_base":      r.cfg.ChatBase,
				"tenant_id":      r.cfg.TenantID,
				"renter_user_id": r.cfg.RenterUserID,
				"api_key_set":    keyOK,
				"api_key":        api.MaskKey(r.cfg.APIKey),
				"session":        sessionSet,
				"config_dir":     r.cfg.Dir,
			}
			if keyErr != "" {
				out["api_key_error"] = keyErr
			}
			if sessionErr != nil {
				out["session_error"] = "No system keychain is available. Hypermesh will not store your session in a file."
			}
			if r.json {
				if err := r.printJSON(out); err != nil {
					return err
				}
				if keyErr != "" {
					return fmt.Errorf("%s", keyErr)
				}
				return nil
			}
			fmt.Fprintf(cmd.OutOrStdout(), "api_base\t%s\n", r.cfg.APIBase)
			fmt.Fprintf(cmd.OutOrStdout(), "chat_base\t%s\n", r.cfg.ChatBase)
			fmt.Fprintf(cmd.OutOrStdout(), "tenant_id\t%s\n", r.cfg.TenantID)
			fmt.Fprintf(cmd.OutOrStdout(), "renter_user_id\t%s\n", r.cfg.RenterUserID)
			if keyOK {
				fmt.Fprintf(cmd.OutOrStdout(), "api_key\t%s\n", api.MaskKey(r.cfg.APIKey))
			} else {
				fmt.Fprintln(cmd.OutOrStdout(), "api_key\t(not set)")
			}
			if sessionSet {
				fmt.Fprintln(cmd.OutOrStdout(), "session\tkeychain")
			} else {
				fmt.Fprintln(cmd.OutOrStdout(), "session\t(not signed in)")
			}
			if keyErr != "" {
				return fmt.Errorf("%s", keyErr)
			}
			return nil
		},
	}
}

func newAuthLogoutCmd(r *run) *cobra.Command {
	return &cobra.Command{
		Use:   "logout",
		Short: "Revoke the session and remove it from the keychain",
		RunE: func(cmd *cobra.Command, args []string) error {
			if err := signOut(session.KeyringStore{}, oauth.Panopticon(), http.DefaultClient, r.cfg.ForgetPlaintextCredentials); err != nil {
				return err
			}
			if r.json {
				return r.printJSON(map[string]any{"ok": true})
			}
			fmt.Fprintln(cmd.OutOrStdout(), "logged out")
			return nil
		},
	}
}
