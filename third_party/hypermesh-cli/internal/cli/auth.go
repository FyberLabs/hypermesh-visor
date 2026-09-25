package cli

import (
	"fmt"

	"github.com/spf13/cobra"

	"github.com/FyberLabs/hypermesh-cli/internal/api"
)

func newAuthCmd(r *run) *cobra.Command {
	cmd := &cobra.Command{
		Use:   "auth",
		Short: "Renter API key login (purpose: renter)",
	}
	cmd.AddCommand(newAuthLoginCmd(r))
	cmd.AddCommand(newAuthWhoamiCmd(r))
	cmd.AddCommand(newAuthLogoutCmd(r))
	return cmd
}

func newAuthLoginCmd(r *run) *cobra.Command {
	var apiKey, tenantID, renterUserID string
	cmd := &cobra.Command{
		Use:   "login",
		Short: "Store an org API key and tenant id (credentials 0600)",
		RunE: func(cmd *cobra.Command, args []string) error {
			if err := api.ValidateRenterKey(apiKey); err != nil {
				return err
			}
			if err := r.cfg.WriteLogin(apiKey, tenantID, renterUserID); err != nil {
				return err
			}
			out := map[string]any{
				"ok":             true,
				"tenant_id":      tenantID,
				"config_dir":     r.cfg.Dir,
				"credentials":    "0600",
				"renter_user_id": renterUserID,
			}
			if r.json {
				return r.printJSON(out)
			}
			fmt.Fprintf(cmd.OutOrStdout(), "logged in tenant %s\ncredentials %s (0600)\n", tenantID, r.cfg.Dir)
			return nil
		},
	}
	cmd.Flags().StringVar(&apiKey, "api-key", "", "org API key from api-keys (purpose: renter)")
	cmd.Flags().StringVar(&tenantID, "tenant-id", "", "tenant id sent as X-Tenant-ID")
	cmd.Flags().StringVar(&renterUserID, "renter-user-id", "", "uuid used as renter_user_id on checkout")
	_ = cmd.MarkFlagRequired("api-key")
	_ = cmd.MarkFlagRequired("tenant-id")
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
			out := map[string]any{
				"api_base":       r.cfg.APIBase,
				"chat_base":      r.cfg.ChatBase,
				"tenant_id":      r.cfg.TenantID,
				"renter_user_id": r.cfg.RenterUserID,
				"api_key_set":    keyOK,
				"api_key":        api.MaskKey(r.cfg.APIKey),
				"config_dir":     r.cfg.Dir,
			}
			if keyErr != "" {
				out["api_key_error"] = keyErr
			}
			if r.json {
				return r.printJSON(out)
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
		Short: "Remove stored credentials",
		RunE: func(cmd *cobra.Command, args []string) error {
			if err := r.cfg.Logout(); err != nil {
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
