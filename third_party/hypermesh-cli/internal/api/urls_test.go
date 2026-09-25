package api

import (
	"strings"
	"testing"
)

func TestJoinBase(t *testing.T) {
	t.Parallel()
	got, err := JoinBase("https://api.test.hyperme.sh/", "/api/v1/hypermesh/catalog")
	if err != nil {
		t.Fatal(err)
	}
	if got != "https://api.test.hyperme.sh/api/v1/hypermesh/catalog" {
		t.Fatalf("got %q", got)
	}
}

func TestLockedAPIURLs(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name string
		fn   func() (string, error)
		want string
	}{
		{"catalog", func() (string, error) { return CatalogURL(DefaultAPIBase) }, "https://api.test.hyperme.sh/api/v1/hypermesh/catalog"},
		{"classes", func() (string, error) { return ClassesURL(DefaultAPIBase) }, "https://api.test.hyperme.sh/api/v1/hypermesh/classes"},
		{"hosts", func() (string, error) { return RenterHostsURL(DefaultAPIBase, "") }, "https://api.test.hyperme.sh/api/v1/hypermesh/renter/hosts"},
		{"leases", func() (string, error) { return LeasesURL(DefaultAPIBase) }, "https://api.test.hyperme.sh/api/v1/hypermesh/leases"},
		{"lease", func() (string, error) { return LeaseURL(DefaultAPIBase, "abc") }, "https://api.test.hyperme.sh/api/v1/hypermesh/leases/abc"},
		{"complete", func() (string, error) { return LeaseCompleteURL(DefaultAPIBase, "abc") }, "https://api.test.hyperme.sh/api/v1/hypermesh/leases/abc/complete"},
		{"chat", func() (string, error) { return ChatCompletionsURL(DefaultChatBase) }, "https://chat.test.hyperme.sh/v1/chat/completions"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got, err := tc.fn()
			if err != nil {
				t.Fatal(err)
			}
			if got != tc.want {
				t.Fatalf("got %q want %q", got, tc.want)
			}
			if strings.Contains(got, PathRenterChatStub) {
				t.Fatal("must never build the renter chat stub path")
			}
		})
	}
}

func TestChatURLNeverUsesAPIBaseStub(t *testing.T) {
	t.Parallel()
	u, err := ChatCompletionsURL(DefaultAPIBase)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(u, "/api/v1/hypermesh/") {
		t.Fatalf("chat must stay on the router completions path, got %q", u)
	}
	if want := "https://api.test.hyperme.sh/v1/chat/completions"; u != want {
		t.Fatalf("got %q want %q", u, want)
	}
}

func TestRenterHostsURLCatalogQueryOnly(t *testing.T) {
	t.Parallel()
	got, err := RenterHostsURL(DefaultAPIBase, DefaultCatalogID)
	if err != nil {
		t.Fatal(err)
	}
	if got != "https://api.test.hyperme.sh/api/v1/hypermesh/renter/hosts?catalog_id=llama-3.1-8b-q4" {
		t.Fatalf("got %q", got)
	}
	blank, err := RenterHostsURL(DefaultAPIBase, "  ")
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(blank, "?") {
		t.Fatalf("empty catalog_id must omit query, got %q", blank)
	}
}

func TestLeaseURLRequiresID(t *testing.T) {
	t.Parallel()
	if _, err := LeaseURL(DefaultAPIBase, ""); err == nil {
		t.Fatal("expected error")
	}
}

func TestJoinBaseRejectsEmpty(t *testing.T) {
	t.Parallel()
	if _, err := JoinBase("", PathCatalog); err == nil {
		t.Fatal("expected error")
	}
}
