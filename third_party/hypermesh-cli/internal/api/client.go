package api

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"

	"github.com/FyberLabs/hypermesh-cli/internal/oauth"
	"github.com/FyberLabs/hypermesh-cli/internal/session"
)

const HeaderAPIKey = "X-Api-Key"
const HeaderTenantID = "X-Tenant-ID"
const HeaderLeaseID = "X-Hypermesh-Lease-Id"
const HeaderLeaseIDAlt = "X-Lease-Id"

type Client struct {
	HTTP        *http.Client
	APIBase     string
	ChatBase    string
	APIKey      string
	TenantID    string
	AccessToken string
	AccessUntil time.Time
	Session     session.Store
	OAuth       oauth.Endpoints
}

func NewClient(apiBase, chatBase, apiKey, tenantID string) *Client {
	return &Client{
		HTTP:     &http.Client{Timeout: 30 * time.Second},
		APIBase:  apiBase,
		ChatBase: chatBase,
		APIKey:   apiKey,
		TenantID: tenantID,
	}
}

type HTTPError struct {
	Method string
	URL    string
	Status int
	Body   string
}

func (e *HTTPError) Error() string {
	msg := strings.TrimSpace(e.Body)
	if len(msg) > 300 {
		msg = msg[:300] + "…"
	}
	if msg == "" {
		return fmt.Sprintf("%s %s: HTTP %d", e.Method, e.URL, e.Status)
	}
	return fmt.Sprintf("%s %s: HTTP %d: %s", e.Method, e.URL, e.Status, msg)
}

func (c *Client) requireRenterAuth() error {
	if err := c.ensureAccess(); err != nil {
		return err
	}
	if c.AccessToken == "" {
		if err := ValidateRenterKey(c.APIKey); err != nil {
			return err
		}
	}
	if strings.TrimSpace(c.TenantID) == "" {
		return fmt.Errorf("tenant id is required (X-Tenant-ID)")
	}
	return nil
}

func (c *Client) ensureAccess() error {
	// An environment API key is the automation path. It does not read the keychain.
	if strings.TrimSpace(c.APIKey) != "" {
		return nil
	}
	if c.AccessToken != "" && time.Now().Before(c.AccessUntil.Add(-15*time.Second)) {
		return nil
	}
	if c.Session == nil {
		return fmt.Errorf("sign in first: hypermesh login")
	}
	refresh, err := c.Session.Refresh()
	if err != nil {
		return err
	}
	if refresh == "" {
		if strings.TrimSpace(c.APIKey) != "" {
			return nil
		}
		return fmt.Errorf("sign in first: hypermesh login")
	}
	if c.HTTP == nil {
		c.HTTP = &http.Client{Timeout: 30 * time.Second}
	}
	tokens, err := oauth.Refresh(c.HTTP, c.OAuth, refresh)
	if err != nil {
		return err
	}
	c.AccessToken = tokens.AccessToken
	c.AccessUntil = time.Now().Add(tokens.ExpiresIn)
	if tokens.RefreshToken != "" && tokens.RefreshToken != refresh {
		if err := c.Session.PutRefresh(tokens.RefreshToken); err != nil {
			return err
		}
	}
	return nil
}

func (c *Client) applyRenterHeaders(req *http.Request) {
	if c.AccessToken != "" {
		req.Header.Set("Authorization", "Bearer "+c.AccessToken)
	} else if c.APIKey != "" {
		req.Header.Set(HeaderAPIKey, c.APIKey)
	}
	if c.TenantID != "" {
		req.Header.Set(HeaderTenantID, c.TenantID)
	}
}

func (c *Client) doJSON(method, rawURL string, body any, auth bool) (json.RawMessage, error) {
	var rdr io.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return nil, err
		}
		rdr = bytes.NewReader(b)
	}
	req, err := http.NewRequest(method, rawURL, rdr)
	if err != nil {
		return nil, err
	}
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	req.Header.Set("Accept", "application/json")
	if auth {
		if err := c.requireRenterAuth(); err != nil {
			return nil, err
		}
	}
	c.applyRenterHeaders(req)
	resp, err := c.HTTP.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	raw, err := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	if err != nil {
		return nil, err
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return nil, &HTTPError{Method: method, URL: rawURL, Status: resp.StatusCode, Body: string(raw)}
	}
	if len(bytes.TrimSpace(raw)) == 0 {
		return json.RawMessage("null"), nil
	}
	if !json.Valid(raw) {
		return nil, fmt.Errorf("%s %s: response is not JSON", method, rawURL)
	}
	return json.RawMessage(raw), nil
}

func (c *Client) GetCatalog() (json.RawMessage, error) {
	u, err := CatalogURL(c.APIBase)
	if err != nil {
		return nil, err
	}
	return c.doJSON(http.MethodGet, u, nil, false)
}

func (c *Client) GetClasses() (json.RawMessage, error) {
	u, err := ClassesURL(c.APIBase)
	if err != nil {
		return nil, err
	}
	return c.doJSON(http.MethodGet, u, nil, false)
}

func (c *Client) GetRenterHosts(catalogID string) (json.RawMessage, error) {
	u, err := RenterHostsURL(c.APIBase, catalogID)
	if err != nil {
		return nil, err
	}
	return c.doJSON(http.MethodGet, u, nil, true)
}

func (c *Client) CreateLease(body LeaseCreate) (Lease, json.RawMessage, error) {
	if err := body.Validate(); err != nil {
		return Lease{}, nil, err
	}
	u, err := LeasesURL(c.APIBase)
	if err != nil {
		return Lease{}, nil, err
	}
	raw, err := c.doJSON(http.MethodPost, u, body, true)
	if err != nil {
		return Lease{}, nil, err
	}
	var lease Lease
	if err := json.Unmarshal(raw, &lease); err != nil {
		return Lease{}, raw, fmt.Errorf("decode lease: %w", err)
	}
	return lease, raw, nil
}

func (c *Client) ListLeases() (json.RawMessage, error) {
	u, err := LeasesURL(c.APIBase)
	if err != nil {
		return nil, err
	}
	return c.doJSON(http.MethodGet, u, nil, true)
}

func (c *Client) GetLease(id string) (Lease, json.RawMessage, error) {
	u, err := LeaseURL(c.APIBase, id)
	if err != nil {
		return Lease{}, nil, err
	}
	raw, err := c.doJSON(http.MethodGet, u, nil, true)
	if err != nil {
		return Lease{}, nil, err
	}
	var lease Lease
	if err := json.Unmarshal(raw, &lease); err != nil {
		return Lease{}, raw, fmt.Errorf("decode lease: %w", err)
	}
	return lease, raw, nil
}

func (c *Client) CompleteLease(id string) (json.RawMessage, error) {
	u, err := LeaseCompleteURL(c.APIBase, id)
	if err != nil {
		return nil, err
	}
	return c.doJSON(http.MethodPost, u, map[string]string{}, true)
}

type ChatMessage struct {
	Role    string `json:"role"`
	Content string `json:"content"`
}

type ChatRequest struct {
	Model    string        `json:"model"`
	Messages []ChatMessage `json:"messages"`
	LeaseID  string        `json:"lease_id,omitempty"`
}

func (c *Client) ChatCompletions(leaseID string, reqBody ChatRequest) (json.RawMessage, error) {
	if err := c.requireRenterAuth(); err != nil {
		return nil, err
	}
	if strings.TrimSpace(leaseID) == "" && strings.TrimSpace(reqBody.LeaseID) == "" {
		return nil, fmt.Errorf("lease id is required for router chat")
	}
	if reqBody.LeaseID == "" {
		reqBody.LeaseID = leaseID
	}
	if strings.TrimSpace(reqBody.Model) == "" {
		reqBody.Model = DefaultCatalogID
	}
	if len(reqBody.Messages) == 0 {
		return nil, fmt.Errorf("messages are required")
	}
	u, err := ChatCompletionsURL(c.ChatBase)
	if err != nil {
		return nil, err
	}
	b, err := json.Marshal(reqBody)
	if err != nil {
		return nil, err
	}
	req, err := http.NewRequest(http.MethodPost, u, bytes.NewReader(b))
	if err != nil {
		return nil, err
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Accept", "application/json")
	c.applyRenterHeaders(req)
	req.Header.Set(HeaderLeaseID, reqBody.LeaseID)
	req.Header.Set(HeaderLeaseIDAlt, reqBody.LeaseID)
	resp, err := c.HTTP.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	raw, err := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	if err != nil {
		return nil, err
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		// Do not echo the prompt. Surface status and a short API error only.
		return nil, &HTTPError{Method: http.MethodPost, URL: u, Status: resp.StatusCode, Body: chatErrorHint(raw)}
	}
	if !json.Valid(raw) {
		return nil, fmt.Errorf("chat completions: response is not JSON")
	}
	return json.RawMessage(raw), nil
}

func chatErrorHint(raw []byte) string {
	var env struct {
		Error struct {
			Message string `json:"message"`
			Type    string `json:"type"`
		} `json:"error"`
		Detail string `json:"detail"`
	}
	if json.Unmarshal(raw, &env) == nil {
		if env.Error.Message != "" {
			return env.Error.Message
		}
		if env.Detail != "" {
			return env.Detail
		}
	}
	return "router rejected the request"
}

func AssistantText(raw json.RawMessage) string {
	var env struct {
		Choices []struct {
			Message struct {
				Content string `json:"content"`
			} `json:"message"`
		} `json:"choices"`
	}
	if json.Unmarshal(raw, &env) != nil {
		return ""
	}
	if len(env.Choices) == 0 {
		return ""
	}
	return env.Choices[0].Message.Content
}
