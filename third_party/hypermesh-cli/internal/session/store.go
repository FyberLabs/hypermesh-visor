// Package session stores the refresh token in the OS keychain.
// The entry is service "hypermesh", account "session", the same entry the
// desktop companion reads. There is no plaintext file fallback.
package session

import (
	"fmt"

	"github.com/zalando/go-keyring"
)

const (
	Service = "hypermesh"
	Account = "session"
)

// Store is the refresh-token entry. Tests use Memory.
type Store interface {
	PutRefresh(token string) error
	Refresh() (string, error)
	Delete() error
}

// KeyringStore uses the macOS Keychain, Windows Credential Manager, or Linux Secret Service.
type KeyringStore struct{}

func (KeyringStore) PutRefresh(token string) error {
	if token == "" {
		return fmt.Errorf("Keycloak did not return a refresh token. The public client must issue a refresh token for this sign-in session.")
	}
	if err := keyring.Set(Service, Account, token); err != nil {
		return noKeychain(err)
	}
	return nil
}

func (KeyringStore) Refresh() (string, error) {
	token, err := keyring.Get(Service, Account)
	if err == keyring.ErrNotFound {
		return "", nil
	}
	if err != nil {
		return "", noKeychain(err)
	}
	return token, nil
}

func (KeyringStore) Delete() error {
	err := keyring.Delete(Service, Account)
	if err == keyring.ErrNotFound || err == nil {
		return nil
	}
	return noKeychain(err)
}

func noKeychain(err error) error {
	return fmt.Errorf("No system keychain is available. Hypermesh will not store your session in a file. (%v)", err)
}

// Memory is the fake keychain used in tests.
type Memory struct {
	token string
	set   bool
}

func (m *Memory) PutRefresh(token string) error {
	if token == "" {
		return fmt.Errorf("empty refresh token")
	}
	m.token = token
	m.set = true
	return nil
}

func (m *Memory) Refresh() (string, error) {
	if !m.set {
		return "", nil
	}
	return m.token, nil
}

func (m *Memory) Delete() error {
	m.token = ""
	m.set = false
	return nil
}
