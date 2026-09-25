package api

import (
	"fmt"
	"net/url"
	"path"
	"strings"
)

const (
	DefaultAPIBase  = "https://api.test.hyperme.sh"
	DefaultChatBase = "https://chat.test.hyperme.sh"

	// Phase 1 Full Model catalog id. Not a certified soak claim.
	DefaultCatalogID = "llama-3.1-8b-q4"

	PathCatalog     = "/api/v1/hypermesh/catalog"
	PathClasses     = "/api/v1/hypermesh/classes"
	PathRenterHosts = "/api/v1/hypermesh/renter/hosts"
	PathLeases      = "/api/v1/hypermesh/leases"

	PathChatCompletions = "/v1/chat/completions"

	// PathRenterChatStub is the always-409 control-plane stub.
	// The CLI must never call it. Chat goes to the Fyber router.
	PathRenterChatStub = "/api/v1/hypermesh/renter/chat/completions"
)

func JoinBase(base, p string) (string, error) {
	if strings.TrimSpace(base) == "" {
		return "", fmt.Errorf("empty base URL")
	}
	u, err := url.Parse(strings.TrimRight(strings.TrimSpace(base), "/"))
	if err != nil {
		return "", fmt.Errorf("parse base URL: %w", err)
	}
	if u.Scheme == "" || u.Host == "" {
		return "", fmt.Errorf("base URL must include scheme and host")
	}
	u.Path = path.Join(u.Path, p)
	if !strings.HasPrefix(u.Path, "/") {
		u.Path = "/" + u.Path
	}
	return u.String(), nil
}

func CatalogURL(apiBase string) (string, error) {
	return JoinBase(apiBase, PathCatalog)
}

func ClassesURL(apiBase string) (string, error) {
	return JoinBase(apiBase, PathClasses)
}

// RenterHostsURL is GET /api/v1/hypermesh/renter/hosts.
// catalogID, when set, is the only query key (catalog_id).
func RenterHostsURL(apiBase, catalogID string) (string, error) {
	u, err := JoinBase(apiBase, PathRenterHosts)
	if err != nil {
		return "", err
	}
	catalogID = strings.TrimSpace(catalogID)
	if catalogID == "" {
		return u, nil
	}
	parsed, err := url.Parse(u)
	if err != nil {
		return "", err
	}
	q := url.Values{}
	q.Set("catalog_id", catalogID)
	parsed.RawQuery = q.Encode()
	return parsed.String(), nil
}

func LeasesURL(apiBase string) (string, error) {
	return JoinBase(apiBase, PathLeases)
}

func LeaseURL(apiBase, id string) (string, error) {
	if strings.TrimSpace(id) == "" {
		return "", fmt.Errorf("lease id is required")
	}
	return JoinBase(apiBase, PathLeases+"/"+url.PathEscape(id))
}

func LeaseCompleteURL(apiBase, id string) (string, error) {
	if strings.TrimSpace(id) == "" {
		return "", fmt.Errorf("lease id is required")
	}
	return JoinBase(apiBase, PathLeases+"/"+url.PathEscape(id)+"/complete")
}

func ChatCompletionsURL(chatBase string) (string, error) {
	u, err := JoinBase(chatBase, PathChatCompletions)
	if err != nil {
		return "", err
	}
	if strings.Contains(u, PathRenterChatStub) {
		return "", fmt.Errorf("refusing renter chat stub path")
	}
	return u, nil
}
