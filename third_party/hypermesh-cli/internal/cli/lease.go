package cli

import (
	"fmt"

	"github.com/spf13/cobra"
)

func newLeaseCmd(r *run) *cobra.Command {
	cmd := &cobra.Command{
		Use:   "lease",
		Short: "List, show, or complete a Hypermesh lease",
	}
	cmd.AddCommand(&cobra.Command{
		Use:   "list",
		Short: "GET /api/v1/hypermesh/leases",
		Args:  cobra.NoArgs,
		RunE: func(cmd *cobra.Command, args []string) error {
			raw, err := r.client.ListLeases()
			if err != nil {
				return err
			}
			if r.json {
				return r.printRawJSON(raw)
			}
			return printIDList(raw)
		},
	})
	cmd.AddCommand(&cobra.Command{
		Use:   "show LEASE_ID",
		Short: "GET /api/v1/hypermesh/leases/{id}",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			lease, raw, err := r.client.GetLease(args[0])
			if err != nil {
				return err
			}
			if r.json {
				return r.printRawJSON(raw)
			}
			fmt.Fprintf(cmd.OutOrStdout(), "id\t%s\nstatus\t%s\ncheckout_url\t%s\n", lease.ID, lease.Status, lease.CheckoutURL)
			return nil
		},
	})
	cmd.AddCommand(&cobra.Command{
		Use:   "complete LEASE_ID",
		Short: "POST /api/v1/hypermesh/leases/{id}/complete",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			raw, err := r.client.CompleteLease(args[0])
			if err != nil {
				return err
			}
			if r.json {
				return r.printRawJSON(raw)
			}
			fmt.Fprintf(cmd.OutOrStdout(), "completed\t%s\n", args[0])
			return nil
		},
	})
	return cmd
}
