package cli

import (
	"bytes"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/FyberLabs/hypermesh-cli/internal/api"
)

func TestPrintHostList(t *testing.T) {
	t.Parallel()
	raw := json.RawMessage(`[{"device_id":"22222222-2222-2222-2222-222222222222","public_label":"agx-large 22222222","class_id":"agx-large","certified":true,"online":false,"sell_state":"selling"}]`)
	var out bytes.Buffer
	if err := printHostList(&out, raw); err != nil {
		t.Fatal(err)
	}
	got := out.String()
	if !strings.Contains(got, "device_id\tpublic_label\tclass_id\tcertified\tonline\tsell_state") {
		t.Fatalf("header: %q", got)
	}
	if !strings.Contains(got, "22222222-2222-2222-2222-222222222222\tagx-large 22222222\tagx-large\ttrue\tfalse\tselling") {
		t.Fatalf("row: %q", got)
	}
}

func TestCheckoutMissingDeviceIDDoesNotPOST(t *testing.T) {
	called := false
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		called = true
		w.WriteHeader(http.StatusCreated)
		_, _ = w.Write([]byte(`{"id":"should-not"}`))
	}))
	defer srv.Close()
	t.Setenv("HYPERMESH_CONFIG_DIR", t.TempDir())
	t.Setenv("HYPERMESH_API_KEY", "org_key")
	t.Setenv("HYPERMESH_TENANT_ID", "ten")
	cmd := New("hypermesh")
	cmd.SetArgs([]string{
		"--api-base", srv.URL,
		"checkout",
		"--renter-user-id", "11111111-1111-1111-1111-111111111111",
		"--success-url", "https://example.test/ok",
		"--cancel-url", "https://example.test/cancel",
		"--no-open",
	})
	cmd.SetOut(io.Discard)
	cmd.SetErr(io.Discard)
	err := cmd.Execute()
	if err == nil {
		t.Fatal("expected missing device_id to fail")
	}
	if !strings.Contains(err.Error(), "device_id") {
		t.Fatalf("got %v", err)
	}
	if called {
		t.Fatal("posted without device_id")
	}
}

func TestCheckoutRejectsPublicLabelBeforePOST(t *testing.T) {
	called := false
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		called = true
	}))
	defer srv.Close()
	t.Setenv("HYPERMESH_CONFIG_DIR", t.TempDir())
	t.Setenv("HYPERMESH_API_KEY", "org_key")
	t.Setenv("HYPERMESH_TENANT_ID", "ten")
	cmd := New("hypermesh")
	cmd.SetArgs([]string{
		"--api-base", srv.URL,
		"checkout",
		"--device-id", "agx-large 22222222",
		"--renter-user-id", "11111111-1111-1111-1111-111111111111",
		"--success-url", "https://example.test/ok",
		"--cancel-url", "https://example.test/cancel",
		"--no-open",
	})
	cmd.SetOut(io.Discard)
	cmd.SetErr(io.Discard)
	err := cmd.Execute()
	if err == nil || !strings.Contains(err.Error(), "public_label") {
		t.Fatalf("got %v", err)
	}
	if called {
		t.Fatal("posted public_label as device_id")
	}
}

func TestHostsListsRenterSafeRows(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != api.PathRenterHosts {
			t.Fatalf("path %s", r.URL.Path)
		}
		if r.URL.Query().Get("catalog_id") != api.DefaultCatalogID {
			t.Fatalf("query %q", r.URL.RawQuery)
		}
		_, _ = w.Write([]byte(`[{"device_id":"22222222-2222-2222-2222-222222222222","public_label":"agx-large 22222222","class_id":"agx-large","certified":true,"online":true,"sell_state":"selling"}]`))
	}))
	defer srv.Close()
	t.Setenv("HYPERMESH_CONFIG_DIR", t.TempDir())
	t.Setenv("HYPERMESH_API_KEY", "org_key")
	t.Setenv("HYPERMESH_TENANT_ID", "ten")
	cmd := New("hypermesh")
	var out bytes.Buffer
	cmd.SetOut(&out)
	cmd.SetErr(io.Discard)
	cmd.SetArgs([]string{"--api-base", srv.URL, "hosts", "--catalog-id", api.DefaultCatalogID})
	if err := cmd.Execute(); err != nil {
		t.Fatal(err)
	}
	got := out.String()
	if !strings.Contains(got, "22222222-2222-2222-2222-222222222222") || !strings.Contains(got, "agx-large 22222222") {
		t.Fatalf("%q", got)
	}
}
