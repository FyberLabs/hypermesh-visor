package cli

import (
	"github.com/spf13/cobra"
)

func newHostsCmd(r *run) *cobra.Command {
	var catalogID string
	cmd := &cobra.Command{
		Use:   "hosts",
		Short: "List renter-safe hosts (GET /api/v1/hypermesh/renter/hosts)",
		Long:  "Same auth as checkout. Rows are device_id (UUID plane id), public_label (display only), class_id, certified, online, sell_state. Pass device_id to checkout; the CLI does not pick a host.",
		Args:  cobra.NoArgs,
		RunE: func(cmd *cobra.Command, args []string) error {
			raw, err := r.client.GetRenterHosts(catalogID)
			if err != nil {
				return err
			}
			if r.json {
				return r.printRawJSON(raw)
			}
			return printHostList(cmd.OutOrStdout(), raw)
		},
	}
	cmd.Flags().StringVar(&catalogID, "catalog-id", "", "optional catalog_id query (only query key the API accepts)")
	return cmd
}
