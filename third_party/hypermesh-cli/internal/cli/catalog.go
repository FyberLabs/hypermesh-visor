package cli

import (
	"github.com/spf13/cobra"
)

func newCatalogCmd(r *run) *cobra.Command {
	cmd := &cobra.Command{
		Use:   "catalog",
		Short: "List the public Full Model catalog (GET /api/v1/hypermesh/catalog)",
		Args:  cobra.NoArgs,
		RunE: func(cmd *cobra.Command, args []string) error {
			raw, err := r.client.GetCatalog()
			if err != nil {
				return err
			}
			if r.json {
				return r.printRawJSON(raw)
			}
			return printIDList(raw)
		},
	}
	cmd.AddCommand(&cobra.Command{
		Use:   "show CATALOG_ID",
		Short: "Show one catalog row (client filter of the public catalog list)",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			raw, err := r.client.GetCatalog()
			if err != nil {
				return err
			}
			item, err := findByID(raw, args[0])
			if err != nil {
				return err
			}
			return r.printJSON(item)
		},
	})
	return cmd
}
