package api

import (
	"encoding/json"
	"testing"
)

func TestLeaseCreateJSON(t *testing.T) {
	t.Parallel()
	body := NewPhase1LeaseCreate(
		"11111111-1111-1111-1111-111111111111",
		"llama-3.1-8b-q4",
		"https://example.test/ok",
		"https://example.test/cancel",
		1,
		"22222222-2222-2222-2222-222222222222",
	)
	raw, err := body.MarshalJSONExact()
	if err != nil {
		t.Fatal(err)
	}
	var got map[string]any
	if err := json.Unmarshal(raw, &got); err != nil {
		t.Fatal(err)
	}
	want := map[string]any{
		"kind":           KindP2LoadedModel,
		"renter_user_id": "11111111-1111-1111-1111-111111111111",
		"catalog_id":     DefaultCatalogID,
		"success_url":    "https://example.test/ok",
		"cancel_url":     "https://example.test/cancel",
		"reserved_hours": float64(1),
		"purpose":        PurposeRenter,
		"device_id":      "22222222-2222-2222-2222-222222222222",
	}
	if len(got) != len(want) {
		t.Fatalf("unexpected keys %v", got)
	}
	for k, v := range want {
		if got[k] != v {
			t.Fatalf("field %s: got %#v want %#v", k, got[k], v)
		}
	}
}

func TestLeaseCreateDefaults(t *testing.T) {
	t.Parallel()
	body := NewPhase1LeaseCreate("u", "", "s", "c", 0, "22222222-2222-2222-2222-222222222222")
	if body.CatalogID != DefaultCatalogID {
		t.Fatalf("catalog default: %q", body.CatalogID)
	}
	if body.ReservedHours != 1 {
		t.Fatalf("hours default: %d", body.ReservedHours)
	}
	if body.Kind != KindP2LoadedModel || body.Purpose != PurposeRenter {
		t.Fatalf("kind/purpose lock: %+v", body)
	}
	if body.DeviceID != "22222222-2222-2222-2222-222222222222" {
		t.Fatalf("device_id: %q", body.DeviceID)
	}
}

func TestLeaseCreateValidate(t *testing.T) {
	t.Parallel()
	body := NewPhase1LeaseCreate("", DefaultCatalogID, "s", "c", 1, "22222222-2222-2222-2222-222222222222")
	if err := body.Validate(); err == nil {
		t.Fatal("expected renter_user_id error")
	}
	body = NewPhase1LeaseCreate("u", DefaultCatalogID, "s", "c", 1, "22222222-2222-2222-2222-222222222222")
	body.Kind = "byom"
	if err := body.Validate(); err == nil {
		t.Fatal("expected kind lock")
	}
	body = NewPhase1LeaseCreate("u", DefaultCatalogID, "s", "c", 1, "")
	if err := body.Validate(); err == nil {
		t.Fatal("expected missing device_id error")
	}
	body = NewPhase1LeaseCreate("u", DefaultCatalogID, "s", "c", 1, "agx-large 22222222")
	if err := body.Validate(); err == nil {
		t.Fatal("expected public_label rejected")
	}
}

func TestWaitTerminal(t *testing.T) {
	t.Parallel()
	if !IsWaitSuccess("active") || !IsWaitFailure("failed") || !IsWaitTerminal("refunded") {
		t.Fatal("status helpers")
	}
	if IsWaitTerminal("offered") || IsWaitTerminal("paid") || IsWaitTerminal("starting") {
		t.Fatal("non-terminal treated as done")
	}
}
