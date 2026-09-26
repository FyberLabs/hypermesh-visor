package cli

import (
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

func TestPromptStreamSendsOnePromptAndHidesTheKey(t *testing.T) {
	const key = "org_fixture_ok"
	const sessionID = "11111111-1111-1111-1111-111111111111"
	var (
		method string
		path   string
		apiKey string
		lease  string
		alt    string
		tenant string
		authz  string
		raw    []byte
	)
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		method = r.Method
		path = r.URL.Path
		apiKey = r.Header.Get("X-Api-Key")
		lease = r.Header.Get("X-Hypermesh-Lease-Id")
		alt = r.Header.Get("X-Lease-Id")
		tenant = r.Header.Get("X-Tenant-ID")
		authz = r.Header.Get("Authorization")
		raw, _ = io.ReadAll(r.Body)
		w.Header().Set("Content-Type", "application/x-ndjson")
		_, _ = io.WriteString(w, `{"kind":"prompt","accepted":true,"echo":"`+key+`"}`+"\n")
	}))
	t.Cleanup(srv.Close)

	dir := t.TempDir()
	cmd := exec.Command(testBinary(t), "prompt", "--visor", srv.URL, "--session", sessionID, "count the sheep")
	cmd.Env = append(os.Environ(),
		"HYPERMESH_CONFIG_DIR="+dir,
		"HYPERMESH_API_KEY="+key,
		"HYPERMESH_TENANT_ID=ten",
		"HYPERMESH_LEASE_ID=lease_should_not_be_sent",
	)
	var stdout, stderr strings.Builder
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr
	if err := cmd.Run(); err != nil {
		t.Fatalf("exit: %v\nstderr: %s", err, stderr.String())
	}
	if method != http.MethodPost || path != "/session/"+sessionID+"/stream" {
		t.Fatalf("request %s %s", method, path)
	}
	if apiKey != key {
		t.Fatalf("X-Api-Key = %q", apiKey)
	}
	if lease != "" || alt != "" || tenant != "" || authz != "" {
		t.Fatalf("extra headers lease=%q alt=%q tenant=%q auth=%q", lease, alt, tenant, authz)
	}
	var posted map[string]any
	if err := json.Unmarshal(bytesTrim(raw), &posted); err != nil {
		t.Fatalf("body %q: %v", raw, err)
	}
	if posted["kind"] != "prompt" || posted["prompt"] != "count the sheep" {
		t.Fatalf("body %#v", posted)
	}
	if _, ok := posted["model"]; ok {
		t.Fatalf("model was sent: %#v", posted)
	}
	if strings.Contains(string(raw), key) {
		t.Fatalf("key stored in the prompt body: %s", raw)
	}
	out := stdout.String()
	errText := stderr.String()
	if strings.Contains(out, key) || strings.Contains(errText, key) {
		t.Fatalf("key in output stdout %q stderr %q", out, errText)
	}
	if !strings.Contains(out, `"accepted":true`) || !strings.Contains(out, "***") {
		t.Fatalf("stdout %q", out)
	}
	if err := filepath.WalkDir(dir, func(path string, d os.DirEntry, err error) error {
		if err != nil || d.IsDir() {
			return err
		}
		b, err := os.ReadFile(path)
		if err != nil {
			return err
		}
		if strings.Contains(string(b), key) {
			t.Fatalf("key stored in %s", path)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
}

func bytesTrim(b []byte) []byte {
	return []byte(strings.TrimSpace(string(b)))
}
