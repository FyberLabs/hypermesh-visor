// Package oauth is the native-app sign-in the CLI shares with the desktop companion.
// The public client has no secret. The refresh token is not kept in this package.
package oauth

import (
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"strings"
	"time"

	"github.com/FyberLabs/hypermesh-cli/internal/session"
)

const (
	PublicClientID = "hypermesh-native"
	Issuer         = "https://auth.test.hyperme.sh/realms/controlplane"
	// Scope is the SSO session only. Offline tokens outlive that session.
	Scope            = "openid"
	SignedInSentence = "You're signed in to Hypermesh. You can close this tab."
	LoginTimeout     = 180 * time.Second
	deviceGrant      = "urn:ietf:params:oauth:grant-type:device_code"
)

// Endpoints are Keycloak OIDC URLs for one realm.
type Endpoints struct {
	AuthorizeURL string
	TokenURL     string
	DeviceURL    string
	RevokeURL    string
	ClientID     string
}

func Panopticon() Endpoints {
	return FromIssuer(Issuer, PublicClientID)
}

func FromIssuer(issuer, clientID string) Endpoints {
	issuer = strings.TrimRight(issuer, "/")
	return Endpoints{
		AuthorizeURL: issuer + "/protocol/openid-connect/auth",
		TokenURL:     issuer + "/protocol/openid-connect/token",
		DeviceURL:    issuer + "/protocol/openid-connect/auth/device",
		RevokeURL:    issuer + "/protocol/openid-connect/revoke",
		ClientID:     clientID,
	}
}

// Tokens stay in memory. Debug and Error strings must not include them.
type Tokens struct {
	AccessToken  string
	RefreshToken string
	ExpiresIn    time.Duration
}

// ErrNoRefreshToken is returned when the token endpoint omits a refresh token.
var ErrNoRefreshToken = errors.New("Keycloak did not return a refresh token. The public client must issue a refresh token for this sign-in session.")

// ErrSessionEnded is returned when Keycloak rejects the refresh token because
// the SSO session is over. The keychain entry is deleted before this is returned
// from RefreshSession.
var ErrSessionEnded = errors.New("Your sign-in ended. Sign in again.")

func (t Tokens) String() string {
	return "oauth.Tokens{redacted}"
}

func (t Tokens) GoString() string {
	return t.String()
}

func NewVerifier() (string, error) {
	return randomToken(32)
}

func NewState() (string, error) {
	return randomToken(24)
}

func ChallengeS256(verifier string) string {
	sum := sha256.Sum256([]byte(verifier))
	return base64.RawURLEncoding.EncodeToString(sum[:])
}

func RedirectURI(port int) string {
	return fmt.Sprintf("http://127.0.0.1:%d/callback", port)
}

func BindLoopback() (net.Listener, string, error) {
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return nil, "", fmt.Errorf("could not bind 127.0.0.1: %w", err)
	}
	port := ln.Addr().(*net.TCPAddr).Port
	return ln, RedirectURI(port), nil
}

func AuthorizationURL(ep Endpoints, redirect, state, challenge string) string {
	q := url.Values{}
	q.Set("response_type", "code")
	q.Set("client_id", ep.ClientID)
	q.Set("redirect_uri", redirect)
	q.Set("scope", Scope)
	q.Set("state", state)
	q.Set("code_challenge", challenge)
	q.Set("code_challenge_method", "S256")
	return ep.AuthorizeURL + "?" + q.Encode()
}

// CallbackCode checks state before trusting the code.
func CallbackCode(rawQuery, expectedState string) (string, error) {
	q, err := url.ParseQuery(rawQuery)
	if err != nil {
		return "", fmt.Errorf("sign-in redirect was not understood")
	}
	if q.Get("state") != expectedState {
		return "", fmt.Errorf("sign-in state did not match")
	}
	if errCode := q.Get("error"); errCode != "" {
		if errCode == "access_denied" {
			return "", fmt.Errorf("sign-in was declined")
		}
		return "", fmt.Errorf("sign-in failed (%s)", errCode)
	}
	code := q.Get("code")
	if code == "" {
		return "", fmt.Errorf("login redirect had no code")
	}
	return code, nil
}

func DisplayAvailable() bool {
	return strings.TrimSpace(os.Getenv("DISPLAY")) != "" || strings.TrimSpace(os.Getenv("WAYLAND_DISPLAY")) != ""
}

func OpenSystemBrowser(rawURL string) error {
	cmd := exec.Command("xdg-open", rawURL)
	if err := cmd.Start(); err != nil {
		return fmt.Errorf("could not open a browser (%w)", err)
	}
	return nil
}

type DeviceCodes struct {
	DeviceCode              string
	UserCode                string
	VerificationURI         string
	VerificationURIComplete string
	Interval                time.Duration
	ExpiresIn               time.Duration
}

func StartDevice(client *http.Client, ep Endpoints) (DeviceCodes, error) {
	form := url.Values{}
	form.Set("client_id", ep.ClientID)
	form.Set("scope", Scope)
	body, err := postForm(client, ep.DeviceURL, form)
	if err != nil {
		return DeviceCodes{}, err
	}
	var raw struct {
		DeviceCode              string `json:"device_code"`
		UserCode                string `json:"user_code"`
		VerificationURI         string `json:"verification_uri"`
		VerificationURIComplete string `json:"verification_uri_complete"`
		Interval                int    `json:"interval"`
		ExpiresIn               int    `json:"expires_in"`
	}
	if err := json.Unmarshal(body, &raw); err != nil {
		return DeviceCodes{}, fmt.Errorf("device authorization was not JSON")
	}
	if raw.DeviceCode == "" || raw.UserCode == "" || raw.VerificationURI == "" || raw.ExpiresIn <= 0 {
		return DeviceCodes{}, fmt.Errorf("device authorization was incomplete")
	}
	interval := time.Duration(raw.Interval) * time.Second
	if interval <= 0 {
		interval = 5 * time.Second
	}
	return DeviceCodes{
		DeviceCode:              raw.DeviceCode,
		UserCode:                raw.UserCode,
		VerificationURI:         raw.VerificationURI,
		VerificationURIComplete: raw.VerificationURIComplete,
		Interval:                interval,
		ExpiresIn:               time.Duration(raw.ExpiresIn) * time.Second,
	}, nil
}

// PollDevice waits, then polls. slow_down adds five seconds to the interval.
// wait is injectable so tests do not sleep.
func PollDevice(client *http.Client, ep Endpoints, deviceCode string, interval, expiresIn time.Duration, wait func(time.Duration)) (Tokens, error) {
	if interval <= 0 {
		interval = 5 * time.Second
	}
	deadline := time.Now().Add(expiresIn)
	for {
		if !time.Now().Before(deadline) {
			return Tokens{}, fmt.Errorf("sign-in expired before it was approved")
		}
		step := interval
		if remaining := time.Until(deadline); step > remaining {
			step = remaining
		}
		if step <= 0 {
			return Tokens{}, fmt.Errorf("sign-in expired before it was approved")
		}
		wait(step)
		if !time.Now().Before(deadline) {
			return Tokens{}, fmt.Errorf("sign-in expired before it was approved")
		}
		form := url.Values{}
		form.Set("grant_type", deviceGrant)
		form.Set("device_code", deviceCode)
		form.Set("client_id", ep.ClientID)
		tokens, oauthErr, err := postToken(client, ep.TokenURL, form)
		if err == nil && oauthErr == "" {
			return tokens, nil
		}
		switch oauthErr {
		case "authorization_pending":
			continue
		case "slow_down":
			interval += 5 * time.Second
			continue
		case "expired_token":
			return Tokens{}, fmt.Errorf("sign-in expired before it was approved")
		case "access_denied":
			return Tokens{}, fmt.Errorf("sign-in was declined")
		}
		if err != nil {
			return Tokens{}, err
		}
	}
}

func ExchangeCode(client *http.Client, ep Endpoints, redirect, code, verifier string) (Tokens, error) {
	form := url.Values{}
	form.Set("grant_type", "authorization_code")
	form.Set("code", code)
	form.Set("redirect_uri", redirect)
	form.Set("client_id", ep.ClientID)
	form.Set("code_verifier", verifier)
	tokens, oauthErr, err := postToken(client, ep.TokenURL, form)
	if oauthErr != "" {
		return Tokens{}, fmt.Errorf("sign-in failed (%s)", oauthErr)
	}
	return tokens, err
}

func Refresh(client *http.Client, ep Endpoints, refreshToken string) (Tokens, error) {
	form := url.Values{}
	form.Set("grant_type", "refresh_token")
	form.Set("refresh_token", refreshToken)
	form.Set("client_id", ep.ClientID)
	tokens, oauthErr, err := postToken(client, ep.TokenURL, form)
	if oauthErr == "invalid_grant" {
		return Tokens{}, ErrSessionEnded
	}
	if oauthErr != "" {
		return Tokens{}, fmt.Errorf("sign-in failed (%s)", oauthErr)
	}
	if err != nil {
		return Tokens{}, err
	}
	if tokens.RefreshToken == "" {
		tokens.RefreshToken = refreshToken
	}
	return tokens, nil
}

// RefreshSession loads the keychain refresh token and exchanges it for an
// access token. invalid_grant means the SSO session ended: the entry is
// deleted and the caller is told to sign in again.
func RefreshSession(client *http.Client, ep Endpoints, store session.Store) (Tokens, error) {
	refreshToken, err := store.Refresh()
	if err != nil {
		return Tokens{}, err
	}
	if refreshToken == "" {
		return Tokens{}, fmt.Errorf("sign in first: hypermesh login")
	}
	tokens, err := Refresh(client, ep, refreshToken)
	if errors.Is(err, ErrSessionEnded) {
		_ = store.Delete()
		return Tokens{}, err
	}
	if err != nil {
		return Tokens{}, err
	}
	if tokens.RefreshToken != "" && tokens.RefreshToken != refreshToken {
		if err := store.PutRefresh(tokens.RefreshToken); err != nil {
			return Tokens{}, err
		}
	}
	return tokens, nil
}

func Revoke(client *http.Client, ep Endpoints, refreshToken string) error {
	form := url.Values{}
	form.Set("token", refreshToken)
	form.Set("token_type_hint", "refresh_token")
	form.Set("client_id", ep.ClientID)
	req, err := http.NewRequest(http.MethodPost, ep.RevokeURL, strings.NewReader(form.Encode()))
	if err != nil {
		return err
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	resp, err := client.Do(req)
	if err != nil {
		return fmt.Errorf("could not revoke the session. It is still in the keychain")
	}
	defer resp.Body.Close()
	raw, _ := io.ReadAll(io.LimitReader(resp.Body, 1<<16))
	if resp.StatusCode >= 200 && resp.StatusCode < 300 {
		return nil
	}
	code := oauthErrorCode(raw)
	if code == "invalid_token" || code == "" && resp.StatusCode == 400 {
		return nil
	}
	if code == "" {
		code = fmt.Sprintf("HTTP %d", resp.StatusCode)
	}
	return fmt.Errorf("could not revoke the session (%s)", code)
}

func AcceptCallback(ln net.Listener, expectedState string, timeout time.Duration) (string, error) {
	deadline := time.Now().Add(timeout)
	_ = ln.(*net.TCPListener).SetDeadline(deadline)
	for {
		conn, err := ln.Accept()
		if err != nil {
			if ne, ok := err.(net.Error); ok && ne.Timeout() {
				return "", fmt.Errorf("sign-in timed out waiting for the browser")
			}
			return "", err
		}
		_ = conn.SetReadDeadline(time.Now().Add(5 * time.Second))
		buf := make([]byte, 8192)
		n, _ := conn.Read(buf)
		req := string(buf[:n])
		path := requestPath(req)
		if !strings.HasPrefix(path, "/callback") {
			writeHTML(conn, 404, "Not found.")
			_ = conn.Close()
			continue
		}
		query := ""
		if i := strings.Index(path, "?"); i >= 0 {
			query = path[i+1:]
		}
		code, err := CallbackCode(query, expectedState)
		if err != nil {
			writeHTML(conn, 400, "Hypermesh couldn't sign you in. "+err.Error()+" You can close this tab.")
			_ = conn.Close()
			return "", err
		}
		writeHTML(conn, 200, SignedInSentence)
		_ = conn.Close()
		return code, nil
	}
}

func postToken(client *http.Client, rawURL string, form url.Values) (Tokens, string, error) {
	if form.Get("client_secret") != "" {
		return Tokens{}, "", fmt.Errorf("public client refuses a client secret")
	}
	body, status, err := postRaw(client, rawURL, form)
	if err != nil {
		return Tokens{}, "", err
	}
	if code := oauthErrorCode(body); code != "" {
		return Tokens{}, code, nil
	}
	if status < 200 || status >= 300 {
		return Tokens{}, "", fmt.Errorf("token request failed with HTTP %d", status)
	}
	var raw struct {
		AccessToken  string `json:"access_token"`
		RefreshToken string `json:"refresh_token"`
		ExpiresIn    int    `json:"expires_in"`
	}
	if err := json.Unmarshal(body, &raw); err != nil || raw.AccessToken == "" {
		return Tokens{}, "", fmt.Errorf("token response was missing access_token")
	}
	expires := time.Duration(raw.ExpiresIn) * time.Second
	if expires <= 0 {
		expires = 60 * time.Second
	}
	return Tokens{AccessToken: raw.AccessToken, RefreshToken: raw.RefreshToken, ExpiresIn: expires}, "", nil
}

func postForm(client *http.Client, rawURL string, form url.Values) ([]byte, error) {
	body, status, err := postRaw(client, rawURL, form)
	if err != nil {
		return nil, err
	}
	if code := oauthErrorCode(body); code != "" {
		return nil, fmt.Errorf("sign-in failed (%s)", code)
	}
	if status < 200 || status >= 300 {
		return nil, fmt.Errorf("token request failed with HTTP %d", status)
	}
	return body, nil
}

func postRaw(client *http.Client, rawURL string, form url.Values) ([]byte, int, error) {
	req, err := http.NewRequest(http.MethodPost, rawURL, strings.NewReader(form.Encode()))
	if err != nil {
		return nil, 0, err
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	resp, err := client.Do(req)
	if err != nil {
		return nil, 0, fmt.Errorf("token request failed")
	}
	defer resp.Body.Close()
	body, err := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	if err != nil {
		return nil, resp.StatusCode, fmt.Errorf("token request failed")
	}
	return body, resp.StatusCode, nil
}

func oauthErrorCode(body []byte) string {
	var raw struct {
		Error string `json:"error"`
	}
	if json.Unmarshal(body, &raw) != nil {
		return ""
	}
	return raw.Error
}

func randomToken(n int) (string, error) {
	buf := make([]byte, n)
	if _, err := rand.Read(buf); err != nil {
		return "", fmt.Errorf("could not read randomness for sign-in: %w", err)
	}
	return base64.RawURLEncoding.EncodeToString(buf), nil
}

func requestPath(req string) string {
	line, _, _ := strings.Cut(req, "\n")
	fields := strings.Fields(line)
	if len(fields) < 2 {
		return "/"
	}
	return fields[1]
}

func writeHTML(conn net.Conn, status int, message string) {
	reason := "Error"
	if status == 200 {
		reason = "OK"
	}
	body := "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Hypermesh</title></head><body><main><p>" +
		htmlEscape(message) + "</p></main></body></html>"
	head := fmt.Sprintf("HTTP/1.1 %d %s\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: %d\r\nconnection: close\r\n\r\n", status, reason, len(body))
	_, _ = io.WriteString(conn, head+body)
}

func htmlEscape(value string) string {
	replacer := strings.NewReplacer("&", "&amp;", "<", "&lt;", ">", "&gt;", "\"", "&quot;")
	return replacer.Replace(value)
}
