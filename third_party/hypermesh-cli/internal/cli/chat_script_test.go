package cli

import (
	"bytes"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"testing"

	"github.com/FyberLabs/hypermesh-cli/internal/api"
)

func TestScriptPromptStdoutIsModelTextOnly(t *testing.T) {
	const modelText = "only the model"
	srv := scriptRouter(t, http.StatusOK, `{"id":"chatcmpl-test","choices":[{"message":{"role":"assistant","content":"`+modelText+`"}}]}`, "hello")
	for _, args := range [][]string{
		{"prompt", "--script", "--lease-id", "lease_1", "hello"},
		{"chat", "--script", "--lease-id", "lease_1", "--message", "hello"},
		{"completions", "create", "--script", "--lease-id", "lease_1", "--message", "hello"},
	} {
		t.Run(strings.Join(args[:2], " "), func(t *testing.T) {
			got := runHypermesh(t, srv.URL, args...)
			if got.Code != 0 {
				t.Fatalf("exit %d stderr %q", got.Code, got.Stderr)
			}
			if got.Stdout != modelText+"\n" {
				t.Fatalf("stdout %q", got.Stdout)
			}
			if got.Stderr != "" {
				t.Fatalf("stderr mixed into the run: %q", got.Stderr)
			}
			for _, leak := range []string{"chatcmpl", "choices", "{", "HTTP", "lease_id", "INFO"} {
				if strings.Contains(got.Stdout, leak) {
					t.Fatalf("stdout contains %q: %q", leak, got.Stdout)
				}
			}
		})
	}
}

func TestScriptPromptKeepsExistingTrailingNewline(t *testing.T) {
	srv := scriptRouter(t, http.StatusOK, `{"choices":[{"message":{"content":"one\ntwo\n"}}]}`, "hello")
	got := runHypermesh(t, srv.URL, "prompt", "--script", "--lease-id", "lease_1", "hello")
	if got.Code != 0 {
		t.Fatalf("exit %d stderr %q", got.Code, got.Stderr)
	}
	if got.Stdout != "one\ntwo\n" {
		t.Fatalf("stdout %q", got.Stdout)
	}
}

func TestScriptFailureExitIsStableAndStdoutEmpty(t *testing.T) {
	const prompt = "UNIQUE_PROMPT_BODY_SHOULD_NOT_LEAK"
	for _, status := range []int{http.StatusPaymentRequired, http.StatusConflict, http.StatusInternalServerError, http.StatusServiceUnavailable} {
		t.Run(fmt.Sprintf("http_%d", status), func(t *testing.T) {
			srv := scriptRouter(t, status, `{"error":{"message":"lease not active"}}`, prompt)
			got := runHypermesh(t, srv.URL, "prompt", "--script", "--lease-id", "lease_1", prompt)
			if got.Code != ExitFailure {
				t.Fatalf("exit %d, want %d; stderr %q stdout %q", got.Code, ExitFailure, got.Stderr, got.Stdout)
			}
			if got.Code == status {
				t.Fatalf("process status followed HTTP %d", status)
			}
			if got.Stdout != "" {
				t.Fatalf("stdout %q", got.Stdout)
			}
			if !strings.Contains(got.Stderr, "lease not active") {
				t.Fatalf("stderr %q", got.Stderr)
			}
			if strings.Contains(got.Stdout, prompt) || strings.Contains(got.Stderr, prompt) {
				t.Fatalf("prompt leaked stdout %q stderr %q", got.Stdout, got.Stderr)
			}
		})
	}
}

func TestScriptMissingMessageExitsNonZero(t *testing.T) {
	got := runHypermesh(t, "", "prompt", "--script", "--lease-id", "lease_1")
	if got.Code != ExitFailure {
		t.Fatalf("exit %d stderr %q", got.Code, got.Stderr)
	}
	if got.Stdout != "" {
		t.Fatalf("stdout %q", got.Stdout)
	}
	if !strings.Contains(got.Stderr, "message is required") {
		t.Fatalf("stderr %q", got.Stderr)
	}
}

func TestScriptRejectsJSONBeforePOST(t *testing.T) {
	called := false
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		called = true
		w.WriteHeader(http.StatusOK)
	}))
	t.Cleanup(srv.Close)
	got := runHypermesh(t, srv.URL, "prompt", "--script", "--json", "--lease-id", "lease_1", "hello")
	if got.Code != ExitFailure {
		t.Fatalf("exit %d stderr %q", got.Code, got.Stderr)
	}
	if got.Stdout != "" {
		t.Fatalf("stdout %q", got.Stdout)
	}
	if !strings.Contains(got.Stderr, "--script") {
		t.Fatalf("stderr %q", got.Stderr)
	}
	if called {
		t.Fatal("posted despite --script --json")
	}
}

func TestScriptEmptyAssistantExitsNonZero(t *testing.T) {
	srv := scriptRouter(t, http.StatusOK, `{"id":"chatcmpl-empty","choices":[{"message":{"content":""}}]}`, "hello")
	got := runHypermesh(t, srv.URL, "chat", "--script", "--lease-id", "lease_1", "--message", "hello")
	if got.Code != ExitFailure {
		t.Fatalf("exit %d stderr %q", got.Code, got.Stderr)
	}
	if got.Stdout != "" {
		t.Fatalf("stdout %q", got.Stdout)
	}
	if strings.Contains(got.Stdout, "chatcmpl-empty") || strings.Contains(got.Stderr, "chatcmpl-empty") {
		t.Fatalf("raw completion leaked stdout %q stderr %q", got.Stdout, got.Stderr)
	}
}

func TestPowerShellWrapperHasNoSecondClient(t *testing.T) {
	b, err := os.ReadFile(filepath.Join(repoRoot(), "scripts", "hypermesh-prompt.ps1"))
	if err != nil {
		t.Fatal(err)
	}
	src := string(b)
	for _, bad := range []string{
		"Invoke-RestMethod",
		"Invoke-WebRequest",
		"HttpClient",
		"System.Net",
		"curl",
		"wget",
		api.PathRenterChatStub,
	} {
		if strings.Contains(src, bad) {
			t.Fatalf("wrapper contains %q", bad)
		}
	}
	for _, need := range []string{"--script", "hypermesh", "LASTEXITCODE", "prompt"} {
		if !strings.Contains(src, need) {
			t.Fatalf("wrapper missing %q", need)
		}
	}
}

func TestPowerShellWrapperStdoutAndExit(t *testing.T) {
	pwsh, err := exec.LookPath("pwsh")
	if err != nil {
		t.Skip("pwsh not installed")
	}
	const modelText = "from the binary"
	srv := scriptRouter(t, http.StatusOK, `{"choices":[{"message":{"content":"`+modelText+`"}}]}`, "hello")
	bin := testBinary(t)
	script := filepath.Join(repoRoot(), "scripts", "hypermesh-prompt.ps1")

	success := runPwsh(t, pwsh, script, []string{
		"-Binary", bin,
		"-ChatBase", srv.URL,
		"-LeaseID", "lease_1",
		"hello",
	}, map[string]string{
		"HYPERMESH_API_KEY":    "org_key",
		"HYPERMESH_TENANT_ID":  "ten",
		"HYPERMESH_CONFIG_DIR": t.TempDir(),
	})
	if success.Code != 0 {
		t.Fatalf("exit %d stderr %q stdout %q", success.Code, success.Stderr, success.Stdout)
	}
	if success.Stdout != modelText+"\n" {
		t.Fatalf("stdout %q", success.Stdout)
	}
	if strings.Contains(success.Stdout, "{") || success.Stderr != "" {
		t.Fatalf("stdout %q stderr %q", success.Stdout, success.Stderr)
	}

	failSrv := scriptRouter(t, http.StatusBadGateway, `{"error":{"message":"router down"}}`, "hello")
	failure := runPwsh(t, pwsh, script, []string{
		"-Binary", bin,
		"-ChatBase", failSrv.URL,
		"-LeaseID", "lease_1",
		"hello",
	}, map[string]string{
		"HYPERMESH_API_KEY":    "org_key",
		"HYPERMESH_TENANT_ID":  "ten",
		"HYPERMESH_CONFIG_DIR": t.TempDir(),
	})
	if failure.Code != ExitFailure {
		t.Fatalf("exit %d stderr %q", failure.Code, failure.Stderr)
	}
	if failure.Stdout != "" {
		t.Fatalf("stdout %q", failure.Stdout)
	}
	if !strings.Contains(failure.Stderr, "router down") {
		t.Fatalf("stderr %q", failure.Stderr)
	}
}

type scriptResult struct {
	Stdout string
	Stderr string
	Code   int
}

func scriptRouter(t *testing.T, status int, body, wantPrompt string) *httptest.Server {
	t.Helper()
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == api.PathRenterChatStub {
			t.Errorf("called control-plane chat stub")
			w.WriteHeader(http.StatusConflict)
			return
		}
		if r.Method != http.MethodPost || r.URL.Path != api.PathChatCompletions {
			t.Errorf("unexpected %s %s", r.Method, r.URL.Path)
			w.WriteHeader(http.StatusNotFound)
			return
		}
		raw, _ := io.ReadAll(r.Body)
		if wantPrompt != "" && !bytes.Contains(raw, []byte(wantPrompt)) {
			t.Errorf("prompt body missing from router request")
		}
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(status)
		_, _ = io.WriteString(w, body)
	}))
	t.Cleanup(srv.Close)
	return srv
}

func runHypermesh(t *testing.T, chatBase string, args ...string) scriptResult {
	t.Helper()
	full := make([]string, 0, len(args)+2)
	if chatBase != "" {
		full = append(full, "--chat-base", chatBase)
	}
	full = append(full, args...)
	cmd := exec.Command(testBinary(t), full...)
	cmd.Env = append(os.Environ(),
		"HYPERMESH_CONFIG_DIR="+t.TempDir(),
		"HYPERMESH_API_KEY=org_key",
		"HYPERMESH_TENANT_ID=ten",
	)
	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr
	err := cmd.Run()
	code := 0
	if err != nil {
		var ee *exec.ExitError
		if !errors.As(err, &ee) {
			t.Fatalf("run: %v\nstderr: %s", err, stderr.String())
		}
		code = ee.ExitCode()
	}
	return scriptResult{Stdout: stdout.String(), Stderr: stderr.String(), Code: code}
}

func runPwsh(t *testing.T, pwsh, script string, args []string, env map[string]string) scriptResult {
	t.Helper()
	argv := []string{"-NoProfile", "-File", script}
	argv = append(argv, args...)
	cmd := exec.Command(pwsh, argv...)
	cmd.Env = os.Environ()
	for k, v := range env {
		cmd.Env = append(cmd.Env, k+"="+v)
	}
	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr
	err := cmd.Run()
	code := 0
	if err != nil {
		var ee *exec.ExitError
		if !errors.As(err, &ee) {
			t.Fatalf("pwsh: %v\nstderr: %s", err, stderr.String())
		}
		code = ee.ExitCode()
	}
	return scriptResult{Stdout: stdout.String(), Stderr: stderr.String(), Code: code}
}

var (
	binOnce sync.Once
	binPath string
	binErr  error
)

func testBinary(t *testing.T) string {
	t.Helper()
	binOnce.Do(func() {
		dir, err := os.MkdirTemp("", "hypermesh-cli-bin-")
		if err != nil {
			binErr = err
			return
		}
		binPath = filepath.Join(dir, "hypermesh")
		cmd := exec.Command("go", "build", "-o", binPath, "./cmd/hypermesh")
		cmd.Dir = repoRoot()
		out, err := cmd.CombinedOutput()
		if err != nil {
			binErr = fmt.Errorf("build cli: %w\n%s", err, out)
		}
	})
	if binErr != nil {
		t.Fatal(binErr)
	}
	return binPath
}

func repoRoot() string {
	_, file, _, ok := runtime.Caller(0)
	if !ok {
		panic("runtime.Caller")
	}
	return filepath.Clean(filepath.Join(filepath.Dir(file), "..", ".."))
}
