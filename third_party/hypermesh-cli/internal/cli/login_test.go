package cli

import (
	"bytes"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/FyberLabs/hypermesh-cli/internal/oauth"
	"github.com/FyberLabs/hypermesh-cli/internal/session"
)

func TestDeviceLoginStoresRefreshTokenOnly(t *testing.T) {
	var mu sync.Mutex
	var seen []string
	n := 0
	responses := []string{
		`{"device_code":"dev-secret","user_code":"ABCD-EFGH","verification_uri":"https://auth.example/device","verification_uri_complete":"https://auth.example/device?user_code=ABCD-EFGH","expires_in":60,"interval":5}`,
		`{"error":"authorization_pending"}`,
		`{"access_token":"access-secret","refresh_token":"refresh-secret","expires_in":30}`,
	}
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		mu.Lock()
		seen = append(seen, string(body))
		i := n
		if i >= len(responses) {
			i = len(responses) - 1
		}
		n++
		payload := responses[i]
		mu.Unlock()
		if strings.Contains(payload, `"error"`) {
			w.WriteHeader(http.StatusBadRequest)
		}
		_, _ = io.WriteString(w, payload)
	}))
	defer srv.Close()
	ep := oauth.Endpoints{DeviceURL: srv.URL + "/device", TokenURL: srv.URL + "/token", ClientID: oauth.PublicClientID}
	store := &session.Memory{}
	var stderr bytes.Buffer
	codes, err := oauth.StartDevice(srv.Client(), ep)
	if err != nil {
		t.Fatal(err)
	}
	_, _ = io.WriteString(&stderr, "Enter "+codes.UserCode+" at "+codes.VerificationURI+"\n")
	tokens, err := oauth.PollDevice(srv.Client(), ep, codes.DeviceCode, codes.Interval, codes.ExpiresIn, func(time.Duration) {})
	if err != nil {
		t.Fatal(err)
	}
	if tokens.AccessToken == tokens.RefreshToken {
		t.Fatal("expected distinct tokens")
	}
	if err := store.PutRefresh(tokens.RefreshToken); err != nil {
		t.Fatal(err)
	}
	got, err := store.Refresh()
	if err != nil || got != "refresh-secret" {
		t.Fatalf("stored %q %v", got, err)
	}
	if got == tokens.AccessToken {
		t.Fatal("access token was stored")
	}
	if !strings.Contains(stderr.String(), "ABCD-EFGH") || !strings.Contains(stderr.String(), "https://auth.example/device") {
		t.Fatalf("stderr %s", stderr.String())
	}
	if strings.Contains(stderr.String(), "dev-secret") || strings.Contains(stderr.String(), "refresh-secret") || strings.Contains(stderr.String(), "access-secret") {
		t.Fatal("secret printed")
	}
	mu.Lock()
	defer mu.Unlock()
	if strings.Contains(strings.Join(seen, "\n"), "client_secret") {
		t.Fatal("client secret was sent")
	}
}

func TestSignOutDeletesTheEntry(t *testing.T) {
	store := &session.Memory{}
	if err := store.PutRefresh("refresh-1"); err != nil {
		t.Fatal(err)
	}
	var hits []string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		hits = append(hits, string(body))
		w.WriteHeader(http.StatusOK)
	}))
	defer srv.Close()
	ep := oauth.Endpoints{RevokeURL: srv.URL, ClientID: oauth.PublicClientID}
	removed := false
	if err := signOut(store, ep, srv.Client(), func() error {
		removed = true
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	got, err := store.Refresh()
	if err != nil || got != "" || !removed {
		t.Fatalf("got %q removed %v err %v", got, removed, err)
	}
	if len(hits) != 1 || !strings.Contains(hits[0], "token=") || strings.Contains(hits[0], "client_secret") {
		t.Fatalf("revoke body %v", hits)
	}
}
