package mcp

import "testing"

func TestBindingsRoundTrip(t *testing.T) {
	store := NewStore(t.TempDir())
	if err := store.Ensure(); err != nil {
		t.Fatal(err)
	}
	if err := store.AddBinding(Binding{
		Server:  "fixture",
		WMClass: "FixtureApp",
	}); err != nil {
		t.Fatal(err)
	}
	file, err := store.LoadBindings()
	if err != nil {
		t.Fatal(err)
	}
	if len(file.Bindings) != 1 || file.Bindings[0].Server != "fixture" {
		t.Fatalf("%+v", file)
	}
}
