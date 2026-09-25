package cli

import (
	"fmt"
	"io"
	"log"
	"os"
	"strings"

	"github.com/spf13/cobra"

	"github.com/FyberLabs/hypermesh-cli/internal/api"
)

type chatOpts struct {
	LeaseID string
	Model   string
	System  string
	Text    string
	Script  bool
}

func newChatCmd(r *run) *cobra.Command {
	var leaseID, model, message, system string
	var script bool
	cmd := &cobra.Command{
		Use:   "chat",
		Short: "POST router /v1/chat/completions (never the control-plane 409 stub)",
		RunE: func(cmd *cobra.Command, args []string) error {
			text, err := messageOrStdin(message, args)
			if err != nil {
				return err
			}
			return runChat(r, cmd, chatOpts{
				LeaseID: leaseID,
				Model:   model,
				System:  system,
				Text:    text,
				Script:  script,
			})
		},
	}
	addChatFlags(cmd, &leaseID, &model, &message, &system, &script)
	return cmd
}

func newPromptCmd(r *run) *cobra.Command {
	var leaseID, model, message, system string
	var script bool
	cmd := &cobra.Command{
		Use:   "prompt [text...]",
		Short: "Non-interactive Full Model prompt; stdout is only the model text",
		Long:  "POST {chat base}/v1/chat/completions. With --script, stdout is only the assistant text and the process exits 1 on failure. Diagnostics stay on stderr. Never calls the control-plane 409 stub.",
		RunE: func(cmd *cobra.Command, args []string) error {
			text, err := messageOrStdin(message, args)
			if err != nil {
				return err
			}
			return runChat(r, cmd, chatOpts{
				LeaseID: leaseID,
				Model:   model,
				System:  system,
				Text:    text,
				Script:  script,
			})
		},
	}
	addChatFlags(cmd, &leaseID, &model, &message, &system, &script)
	return cmd
}

func newCompletionsCmd(r *run) *cobra.Command {
	cmd := &cobra.Command{
		Use:   "completions",
		Short: "OpenAI-shaped chat completions against the Fyber router",
	}
	var leaseID, model, message, system string
	var script bool
	create := &cobra.Command{
		Use:   "create",
		Short: "POST https://chat.test.hyperme.sh/v1/chat/completions",
		RunE: func(cmd *cobra.Command, args []string) error {
			text, err := messageOrStdin(message, args)
			if err != nil {
				return err
			}
			return runChat(r, cmd, chatOpts{
				LeaseID: leaseID,
				Model:   model,
				System:  system,
				Text:    text,
				Script:  script,
			})
		},
	}
	addChatFlags(create, &leaseID, &model, &message, &system, &script)
	cmd.AddCommand(create)
	return cmd
}

func addChatFlags(cmd *cobra.Command, leaseID, model, message, system *string, script *bool) {
	cmd.Flags().StringVar(leaseID, "lease-id", "", "paid lease ticket (also HYPERMESH_LEASE_ID)")
	cmd.Flags().StringVar(model, "model", api.DefaultCatalogID, "OpenAI-shaped model name (Phase 1 catalog id)")
	cmd.Flags().StringVar(message, "message", "", "user message (omit to read remaining args or stdin)")
	cmd.Flags().StringVar(system, "system", "", "optional system message")
	cmd.Flags().BoolVar(script, "script", false, "non-interactive: stdout is only the model text; exit 1 on failure")
}

func messageOrStdin(flag string, args []string) (string, error) {
	if strings.TrimSpace(flag) != "" {
		return flag, nil
	}
	if len(args) > 0 {
		return strings.Join(args, " "), nil
	}
	stat, err := os.Stdin.Stat()
	if err == nil && (stat.Mode()&os.ModeCharDevice) == 0 {
		b, err := io.ReadAll(io.LimitReader(os.Stdin, 1<<20))
		if err != nil {
			return "", err
		}
		text := strings.TrimSpace(string(b))
		if text != "" {
			return text, nil
		}
	}
	return "", fmt.Errorf("message is required (--message, args, or stdin)")
}

func runChat(r *run, cmd *cobra.Command, opt chatOpts) error {
	if opt.Script && r.json {
		return fmt.Errorf("--script cannot be combined with --json")
	}
	if opt.Script {
		// Pin the logger to stderr for this call so a script capture of
		// stdout cannot pick up diagnostics.
		log.SetOutput(cmd.ErrOrStderr())
	}
	leaseID := opt.LeaseID
	if leaseID == "" {
		leaseID = r.cfg.LeaseID
	}
	var messages []api.ChatMessage
	if strings.TrimSpace(opt.System) != "" {
		messages = append(messages, api.ChatMessage{Role: "system", Content: opt.System})
	}
	messages = append(messages, api.ChatMessage{Role: "user", Content: opt.Text})
	raw, err := r.client.ChatCompletions(leaseID, api.ChatRequest{
		Model:    opt.Model,
		Messages: messages,
		LeaseID:  leaseID,
	})
	if err != nil {
		return err
	}
	if r.json {
		return r.printRawJSON(raw)
	}
	out := api.AssistantText(raw)
	if strings.TrimSpace(out) == "" {
		return fmt.Errorf("chat completions: empty assistant text")
	}
	return writeModelText(cmd.OutOrStdout(), out)
}

// writeModelText writes the assistant content and nothing else.
// A single trailing newline is added when the model text has none.
func writeModelText(w io.Writer, text string) error {
	if !strings.HasSuffix(text, "\n") {
		text += "\n"
	}
	_, err := io.WriteString(w, text)
	return err
}
