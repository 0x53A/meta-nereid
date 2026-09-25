CC ?= cc
PKG_CONFIG ?= pkg-config
CPPFLAGS += -Isrc $(shell $(PKG_CONFIG) --cflags gio-unix-2.0)
CFLAGS ?= -O2 -g
CFLAGS += -std=gnu11 -Wall -Wextra -Werror
LDLIBS += $(shell $(PKG_CONFIG) --libs gio-unix-2.0)
BINDIR ?= /usr/bin
.PHONY: all check install clean
all: build/hoki-nfc-test-card
build:
	mkdir -p $@
build/hoki-nfc-test-card: src/main.c src/card.c src/card.h | build
	$(CC) $(CPPFLAGS) $(CFLAGS) src/main.c src/card.c $(LDFLAGS) $(LDLIBS) -o $@
build/test-card: tests/test_card.c src/card.c src/card.h | build
	$(CC) $(CPPFLAGS) $(CFLAGS) tests/test_card.c src/card.c $(LDFLAGS) $(LDLIBS) -o $@
build/test-dbus: tests/test_dbus.c src/card.h | build
	$(CC) $(CPPFLAGS) $(CFLAGS) tests/test_dbus.c $(LDFLAGS) $(LDLIBS) -o $@
check: all build/test-card build/test-dbus
	./build/test-card
	./build/test-dbus
install: all
	install -d $(DESTDIR)$(BINDIR)
	install -m 0755 build/hoki-nfc-test-card $(DESTDIR)$(BINDIR)/
clean:
	$(RM) build/hoki-nfc-test-card build/test-card build/test-dbus
