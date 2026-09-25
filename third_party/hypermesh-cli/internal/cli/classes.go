package cli

import "github.com/spf13/cobra"

func newClassesCmd(r *run) *cobra.Command {
	return &cobra.Command{
		Use:   "classes",
		Short: "List public hardware classes (GET /api/v1/hypermesh/classes)",
		Args:  cobra.NoArgs,
		RunE: func(cmd *cobra.Command, args []string) error {
			raw, err := r.client.GetClasses()
			if err != nil {
				return err
			}
			if r.json {
				return r.printRawJSON(raw)
			}
			return printIDList(raw)
		},
	}
}
