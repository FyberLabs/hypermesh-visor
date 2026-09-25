package oauth

import (
	"crypto/sha256"
	"encoding/base64"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"
)

func TestChallengeS256RFC7636(t *testing.T) {
	verifier := "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
	sum := sha256.Sum256([]byte(verifier))
	want := base64.RawURLEncoding.EncodeToString(sum[:])
	if got := ChallengeS256(verifier); got != want || got != "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM" {
		t.Fatalf("challenge %s", got)
	}
}

func TestRedirectURIUsesEphemeralPort(t *testing.T) {
	ln, uri, err := BindLoopback()
	if err != nil {
		t.Fatal(err)
	}
	defer ln.Close()
	port := ln.Addr().(*net.TCPAddr).Port
	if port == 0 || uri != RedirectURI(port) {
		t.Fatalf("uri %s port %d", uri, port)
	}
	if RedirectURI(49152) != "http://127.0.0.1:49152/callback" {
		t.Fatal(RedirectURI(49152))
	}
}

func TestStateMismatch(t *testing.T) {
	_, err := CallbackCode("code=secret-code&state=other", "expected")
	if err == nil || !strings.Contains(err.Error(), "state") || strings.Contains(err.Error(), "secret-code") {
		t.Fatal(err)
	}
	code, err := CallbackCode("code=secret-code&state=expected", "expected")
	if err != nil || code != "secret-code" {
		t.Fatalf("%s %v", code, err)
	}
}

func TestDevicePollPendingSlowDownExpired(t *testing.T) {
	var mu sync.Mutex
	var bodies []string
	step := 0
	pending := []string{
		`{"error":"authorization_pending"}`,
		`{"access_token":"access-1","refresh_token":"refresh-1","expires_in":30}`,
	}
	srv := scriptServer(t, &mu, &bodies, &step, pending)
	defer srv.Close()
	ep := Endpoints{TokenURL: srv.URL, ClientID: PublicClientID}
	var slept []time.Duration
	tokens, err := PollDevice(srv.Client(), ep, "device-1", 5*time.Second, time.Minute, func(d time.Duration) {
		slept = append(slept, d)
	})
	if err != nil || tokens.RefreshToken != "refresh-1" {
		t.Fatalf("%+v %v", tokens, err)
	}
	if len(slept) != 2 || slept[0] != 5*time.Second || slept[1] != 5*time.Second {
		t.Fatalf("slept %v", slept)
	}

	step = 0
	bodies = nil
	slow := scriptServer(t, &mu, &bodies, &step, []string{
		`{"error":"slow_down"}`,
		`{"access_token":"access-2","refresh_token":"refresh-2","expires_in":30}`,
	})
	defer slow.Close()
	ep.TokenURL = slow.URL
	slept = nil
	if _, err := PollDevice(slow.Client(), ep, "device-2", 5*time.Second, time.Minute, func(d time.Duration) {
		slept = append(slept, d)
	}); err != nil {
		t.Fatal(err)
	}
	if len(slept) != 2 || slept[1] != 10*time.Second {
		t.Fatalf("slow slept %v", slept)
	}

	step = 0
	expired := scriptServer(t, &mu, &bodies, &step, []string{`{"error":"expired_token"}`})
	defer expired.Close()
	ep.TokenURL = expired.URL
	_, err = PollDevice(expired.Client(), ep, "device-3", 5*time.Second, time.Minute, func(time.Duration) {})
	if err == nil || strings.Contains(err.Error(), "device-3") {
		t.Fatal(err)
	}
	mu.Lock()
	defer mu.Unlock()
	for _, body := range bodies {
		if strings.Contains(body, "client_secret") {
			t.Fatal("client secret was sent")
		}
	}
}

func scriptServer(t *testing.T, mu *sync.Mutex, bodies *[]string, step *int, responses []string) *httptest.Server {
	t.Helper()
	return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		raw, _ := io.ReadAll(r.Body)
		mu.Lock()
		*bodies = append(*bodies, string(raw))
		i := *step
		if i >= len(responses) {
			i = len(responses) - 1
		}
		*step++
		body := responses[i]
		mu.Unlock()
		if strings.Contains(body, `"error"`) {
			w.WriteHeader(http.StatusBadRequest)
		}
		_, _ = io.WriteString(w, body)
	}))
}
