package api

import (
	"context"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// ControlSocket is control.sock in the visor runtime directory.
func ControlSocket() (string, error) {
	dir, err := runtimeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, "control.sock"), nil
}

// VisorToken is HYPERMESH_VISOR_TOKEN, or the api.token file when that is unset.
func VisorToken() string {
	if token := strings.TrimSpace(os.Getenv("HYPERMESH_VISOR_TOKEN")); token != "" {
		return token
	}
	dir, err := runtimeDir()
	if err != nil {
		return ""
	}
	raw, err := os.ReadFile(filepath.Join(dir, "api.token"))
	if err != nil {
		return ""
	}
	return strings.TrimSpace(string(raw))
}

func runtimeDir() (string, error) {
	if dir := strings.TrimSpace(os.Getenv("HYPERMESH_RUNTIME_DIR")); dir != "" {
		return dir, nil
	}
	if dir := strings.TrimSpace(os.Getenv("XDG_RUNTIME_DIR")); dir != "" {
		return filepath.Join(dir, "hypermesh"), nil
	}
	if dir := strings.TrimSpace(os.Getenv("XDG_STATE_HOME")); dir != "" {
		return filepath.Join(dir, "hypermesh", "run"), nil
	}
	home, err := os.UserHomeDir()
	if err != nil || strings.TrimSpace(home) == "" {
		return "", errString("visor socket needs HYPERMESH_RUNTIME_DIR, XDG_RUNTIME_DIR, XDG_STATE_HOME, or HOME")
	}
	return filepath.Join(home, ".local", "state", "hypermesh", "run"), nil
}

func errString(text string) error {
	return &localError{text}
}

type localError struct{ text string }

func (e *localError) Error() string { return e.text }

func unixClient(socket string) *http.Client {
	return &http.Client{
		Timeout: 30 * time.Second,
		Transport: &http.Transport{
			DialContext: func(ctx context.Context, _, _ string) (net.Conn, error) {
				var dialer net.Dialer
				return dialer.DialContext(ctx, "unix", socket)
			},
		},
	}
}
