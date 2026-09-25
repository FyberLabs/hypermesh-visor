package api

import (
	"encoding/json"
	"fmt"
	"regexp"
	"strings"
)

// deviceIDRe is a UUID (plane id). public_label is not accepted.
var deviceIDRe = regexp.MustCompile(`(?i)^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`)

const (
	KindP2LoadedModel = "p2_loaded_model"
	PurposeRenter     = "renter"
)

// LeaseCreate is the locked Phase 1 Full Model checkout body.
type LeaseCreate struct {
	Kind          string `json:"kind"`
	RenterUserID  string `json:"renter_user_id"`
	CatalogID     string `json:"catalog_id"`
	SuccessURL    string `json:"success_url"`
	CancelURL     string `json:"cancel_url"`
	ReservedHours int    `json:"reserved_hours"`
	Purpose       string `json:"purpose"`
	DeviceID      string `json:"device_id"`
}

// Lease is the subset the CLI depends on. Extra fields pass through as JSON.
type Lease struct {
	ID          string `json:"id"`
	Status      string `json:"status"`
	CheckoutURL string `json:"checkout_url"`
}

func NewPhase1LeaseCreate(renterUserID, catalogID, successURL, cancelURL string, reservedHours int, deviceID string) LeaseCreate {
	if catalogID == "" {
		catalogID = DefaultCatalogID
	}
	if reservedHours <= 0 {
		reservedHours = 1
	}
	return LeaseCreate{
		Kind:          KindP2LoadedModel,
		RenterUserID:  renterUserID,
		CatalogID:     catalogID,
		SuccessURL:    successURL,
		CancelURL:     cancelURL,
		ReservedHours: reservedHours,
		Purpose:       PurposeRenter,
		DeviceID:      strings.TrimSpace(deviceID),
	}
}

func IsDeviceID(s string) bool {
	return deviceIDRe.MatchString(strings.TrimSpace(s))
}

func (c LeaseCreate) Validate() error {
	if c.Kind != KindP2LoadedModel {
		return fmt.Errorf("phase 1 checkout kind must be %s", KindP2LoadedModel)
	}
	if c.Purpose != PurposeRenter {
		return fmt.Errorf("phase 1 checkout purpose must be %s", PurposeRenter)
	}
	if strings.TrimSpace(c.RenterUserID) == "" {
		return fmt.Errorf("renter_user_id is required")
	}
	if strings.TrimSpace(c.CatalogID) == "" {
		return fmt.Errorf("catalog_id is required")
	}
	if strings.TrimSpace(c.SuccessURL) == "" {
		return fmt.Errorf("success_url is required")
	}
	if strings.TrimSpace(c.CancelURL) == "" {
		return fmt.Errorf("cancel_url is required")
	}
	if c.ReservedHours <= 0 {
		return fmt.Errorf("reserved_hours must be >= 1")
	}
	if c.Purpose == PurposeRenter {
		if strings.TrimSpace(c.DeviceID) == "" {
			return fmt.Errorf("device_id is required for renter checkout (UUID from hosts; not public_label)")
		}
		if !IsDeviceID(c.DeviceID) {
			return fmt.Errorf("device_id must be a UUID (plane id); public_label is display only")
		}
	}
	return nil
}

func (c LeaseCreate) MarshalJSONExact() ([]byte, error) {
	if err := c.Validate(); err != nil {
		return nil, err
	}
	return json.Marshal(c)
}

func IsWaitSuccess(status string) bool {
	return status == "active"
}

func IsWaitFailure(status string) bool {
	switch status {
	case "failed", "refunded":
		return true
	default:
		return false
	}
}

func IsWaitTerminal(status string) bool {
	return IsWaitSuccess(status) || IsWaitFailure(status) || status == "ended"
}
