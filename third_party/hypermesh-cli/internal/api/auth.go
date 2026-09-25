package api

import (
	"fmt"
	"strings"
)

// Forbidden renter identity prefixes. These are host / router / site
// credentials, never a renter API key.
var ForbiddenRenterPrefixes = []string{"hm_dev_", "hm_rtr_", "hm_site_"}

func ValidateRenterKey(key string) error {
	key = strings.TrimSpace(key)
	if key == "" {
		return fmt.Errorf("api key is required")
	}
	for _, p := range ForbiddenRenterPrefixes {
		if strings.HasPrefix(key, p) {
			return fmt.Errorf("%s is not a renter identity; use an org API key (purpose: renter)", p)
		}
	}
	return nil
}

func MaskKey(key string) string {
	if key == "" {
		return ""
	}
	if len(key) <= 4 {
		return "****"
	}
	return "****" + key[len(key)-4:]
}
