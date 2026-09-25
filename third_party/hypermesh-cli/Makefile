.PHONY: build test install clean

PREFIX ?= /usr/local
BINDIR ?= $(PREFIX)/bin

build:
	mkdir -p bin
	go build -o bin/hypermesh ./cmd/hypermesh
	go build -o bin/hm ./cmd/hm

test:
	go test ./...

install: build
	install -d $(DESTDIR)$(BINDIR)
	install -m 755 bin/hypermesh $(DESTDIR)$(BINDIR)/hypermesh
	install -m 755 bin/hm $(DESTDIR)$(BINDIR)/hm

clean:
	rm -rf bin
