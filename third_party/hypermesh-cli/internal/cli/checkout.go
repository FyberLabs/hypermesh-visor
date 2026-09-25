package cli

import (
	"fmt"
	"os"
	"os/exec"
	"runtime"
	"time"

	"github.com/spf13/cobra"

	"github.com/FyberLabs/hypermesh-cli/internal/api"
)

func newCheckoutCmd(r *run) *cobra.Command {
	var (
		catalogID     string
		renterUserID  string
		deviceID      string
		successURL    string
		cancelURL     string
		reservedHours int
		wait          bool
		noOpen        bool
		pollEvery     time.Duration
		waitTimeout   time.Duration
	)
	cmd := &cobra.Command{
		Use:   "checkout",
		Short: "Create a Phase 1 Full Model lease and Stripe test Checkout",
		Long:  "POST /api/v1/hypermesh/leases with kind=p2_loaded_model, catalog_id=llama-3.1-8b-q4, purpose=renter, and device_id (UUID from hosts). Missing device_id fails before POST. Prints lease id and checkout_url immediately. Stripe Checkout only.",
		RunE: func(cmd *cobra.Command, args []string) error {
			if renterUserID == "" {
				renterUserID = r.cfg.RenterUserID
			}
			if successURL == "" {
				successURL = r.cfg.SuccessURL
			}
			if cancelURL == "" {
				cancelURL = r.cfg.CancelURL
			}
			body := api.NewPhase1LeaseCreate(renterUserID, catalogID, successURL, cancelURL, reservedHours, deviceID)
			lease, raw, err := r.client.CreateLease(body)
			if err != nil {
				return err
			}
			fmt.Fprintf(os.Stderr, "lease_id\t%s\n", lease.ID)
			fmt.Fprintf(os.Stderr, "checkout_url\t%s\n", lease.CheckoutURL)
			if !noOpen && lease.CheckoutURL != "" {
				if err := openURL(lease.CheckoutURL); err != nil {
					fmt.Fprintf(os.Stderr, "open checkout_url failed: %v\n", err)
				}
			}
			if wait {
				var werr error
				lease, raw, werr = pollLease(r, lease.ID, pollEvery, waitTimeout)
				if werr != nil {
					return werr
				}
			}
			if r.json {
				return r.printRawJSON(raw)
			}
			fmt.Fprintf(cmd.OutOrStdout(), "id\t%s\nstatus\t%s\ncheckout_url\t%s\n", lease.ID, lease.Status, lease.CheckoutURL)
			if wait && api.IsWaitFailure(lease.Status) {
				return fmt.Errorf("lease %s ended %s", lease.ID, lease.Status)
			}
			return nil
		},
	}
	cmd.Flags().StringVar(&catalogID, "catalog-id", api.DefaultCatalogID, "Full Model catalog id")
	cmd.Flags().StringVar(&deviceID, "device-id", "", "host device_id UUID from hosts (not public_label)")
	cmd.Flags().StringVar(&renterUserID, "renter-user-id", "", "renter user uuid")
	cmd.Flags().StringVar(&successURL, "success-url", "", "Stripe Checkout success URL")
	cmd.Flags().StringVar(&cancelURL, "cancel-url", "", "Stripe Checkout cancel URL")
	cmd.Flags().IntVar(&reservedHours, "reserved-hours", 1, "reserved hours (Phase 1 default 1)")
	cmd.Flags().BoolVar(&wait, "wait", false, "poll GET /leases/{id} until active|failed")
	cmd.Flags().BoolVar(&noOpen, "no-open", false, "do not open checkout_url")
	cmd.Flags().DurationVar(&pollEvery, "poll-interval", 2*time.Second, "lease poll interval with --wait")
	cmd.Flags().DurationVar(&waitTimeout, "wait-timeout", 15*time.Minute, "give up waiting after this duration")
	return cmd
}

func pollLease(r *run, id string, every, timeout time.Duration) (api.Lease, []byte, error) {
	if every <= 0 {
		every = 2 * time.Second
	}
	deadline := time.Now().Add(timeout)
	var last api.Lease
	var raw []byte
	for {
		lease, body, err := r.client.GetLease(id)
		if err != nil {
			return last, raw, err
		}
		last, raw = lease, body
		fmt.Fprintf(os.Stderr, "status\t%s\n", lease.Status)
		if api.IsWaitTerminal(lease.Status) {
			if api.IsWaitFailure(lease.Status) {
				return lease, raw, fmt.Errorf("lease %s %s", id, lease.Status)
			}
			return lease, raw, nil
		}
		if timeout > 0 && time.Now().After(deadline) {
			return lease, raw, fmt.Errorf("timed out waiting for lease %s (last status %s)", id, lease.Status)
		}
		time.Sleep(every)
	}
}

func openURL(u string) error {
	var cmd *exec.Cmd
	switch runtime.GOOS {
	case "darwin":
		cmd = exec.Command("open", u)
	case "windows":
		cmd = exec.Command("rundll32", "url.dll,FileProtocolHandler", u)
	default:
		cmd = exec.Command("xdg-open", u)
	}
	return cmd.Start()
}
