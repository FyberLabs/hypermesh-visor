package cli

import (
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"strings"
	"time"

	"github.com/spf13/cobra"

	"github.com/FyberLabs/hypermesh-cli/internal/oauth"
	"github.com/FyberLabs/hypermesh-cli/internal/session"
)

func newLoginCmd(r *run) *cobra.Command {
	var device bool
	cmd := &cobra.Command{
		Use:   "login",
		Short: "Sign in with the system browser, or a device code",
		Long:  "Sign in opens your browser; after you approve, Hypermesh stores your session in your system keychain. On a machine without a browser, use `hypermesh login --device`.",
		RunE: func(cmd *cobra.Command, args []string) error {
			return signIn(cmd.OutOrStdout(), cmd.ErrOrStderr(), session.KeyringStore{}, oauth.Panopticon(), http.DefaultClient, device, oauth.DisplayAvailable(), oauth.OpenSystemBrowser, r.cfg.ForgetPlaintextCredentials, r.noteTenant)
		},
	}
	cmd.Flags().BoolVar(&device, "device", false, "print a device code instead of opening a browser")
	return cmd
}

func newLogoutCmd(r *run) *cobra.Command {
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

func signIn(stdout, stderr io.Writer, store session.Store, ep oauth.Endpoints, client *http.Client, forceDevice, hasDisplay bool, open func(string) error, forget func() error, noteTenant func(accessToken string) error) error {
	var tokens oauth.Tokens
	var err error
	if forceDevice || !hasDisplay {
		tokens, err = deviceSignIn(stderr, ep, client)
	} else {
		tokens, err = browserSignIn(ep, client, open)
		if err != nil && strings.Contains(err.Error(), "could not open a browser") {
			tokens, err = deviceSignIn(stderr, ep, client)
		}
	}
	if err != nil {
		return err
	}
	if tokens.RefreshToken == "" || tokens.RefreshToken == tokens.AccessToken {
		return oauth.ErrNoRefreshToken
	}
	if err := store.PutRefresh(tokens.RefreshToken); err != nil {
		return err
	}
	if forget != nil {
		if err := forget(); err != nil {
			return err
		}
	}
	if noteTenant != nil {
		if err := noteTenant(tokens.AccessToken); err != nil {
			return err
		}
	}
	fmt.Fprintln(stdout, "Signed in. Hypermesh stored your session in the system keychain.")
	return nil
}

func browserSignIn(ep oauth.Endpoints, client *http.Client, open func(string) error) (oauth.Tokens, error) {
	ln, redirect, err := oauth.BindLoopback()
	if err != nil {
		return oauth.Tokens{}, err
	}
	defer ln.Close()
	verifier, err := oauth.NewVerifier()
	if err != nil {
		return oauth.Tokens{}, err
	}
	state, err := oauth.NewState()
	if err != nil {
		return oauth.Tokens{}, err
	}
	rawURL := oauth.AuthorizationURL(ep, redirect, state, oauth.ChallengeS256(verifier))
	if err := open(rawURL); err != nil {
		return oauth.Tokens{}, err
	}
	code, err := oauth.AcceptCallback(ln, state, oauth.LoginTimeout)
	if err != nil {
		return oauth.Tokens{}, err
	}
	return oauth.ExchangeCode(client, ep, redirect, code, verifier)
}

func deviceSignIn(stderr io.Writer, ep oauth.Endpoints, client *http.Client) (oauth.Tokens, error) {
	codes, err := oauth.StartDevice(client, ep)
	if err != nil {
		return oauth.Tokens{}, err
	}
	fmt.Fprintf(stderr, "Enter %s at %s\n", codes.UserCode, codes.VerificationURI)
	if codes.VerificationURIComplete != "" {
		fmt.Fprintf(stderr, "%s\n", codes.VerificationURIComplete)
	}
	return oauth.PollDevice(client, ep, codes.DeviceCode, codes.Interval, codes.ExpiresIn, time.Sleep)
}

func (r *run) noteTenant(access string) error {
	if strings.TrimSpace(access) == "" || strings.TrimSpace(r.cfg.TenantID) != "" {
		return nil
	}
	id, err := firstTenant(http.DefaultClient, r.cfg.APIBase, access)
	if err != nil {
		fmt.Fprintf(os.Stderr, "signed in, but the tenant id was not saved\n")
		return nil
	}
	return r.cfg.WriteProfile(id, r.cfg.RenterUserID)
}

func firstTenant(client *http.Client, apiBase, access string) (string, error) {
	endpoint := strings.TrimRight(apiBase, "/") + "/api/v1/tenants"
	req, err := http.NewRequest(http.MethodGet, endpoint, nil)
	if err != nil {
		return "", err
	}
	req.Header.Set("Authorization", "Bearer "+access)
	req.Header.Set("Accept", "application/json")
	resp, err := client.Do(req)
	if err != nil {
		return "", err
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return "", fmt.Errorf("tenant lookup failed with HTTP %d", resp.StatusCode)
	}
	var raw struct {
		Tenants []struct {
			ID string `json:"id"`
		} `json:"tenants"`
	}
	if err := json.NewDecoder(io.LimitReader(resp.Body, 1<<20)).Decode(&raw); err != nil {
		return "", err
	}
	if len(raw.Tenants) == 0 || strings.TrimSpace(raw.Tenants[0].ID) == "" {
		return "", fmt.Errorf("no tenant on the signed-in account")
	}
	return raw.Tenants[0].ID, nil
}

func signOut(store session.Store, ep oauth.Endpoints, client *http.Client, forget func() error) error {
	refresh, err := store.Refresh()
	if err != nil {
		return err
	}
	if refresh != "" {
		if err := oauth.Revoke(client, ep, refresh); err != nil {
			return err
		}
	}
	if err := store.Delete(); err != nil {
		return err
	}
	if forget != nil {
		return forget()
	}
	return nil
}
