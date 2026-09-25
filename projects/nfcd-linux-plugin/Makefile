CC ?= cc
PKG_CONFIG ?= pkg-config
PKGS = gio-unix-2.0 libglibutil libnl-genl-3.0
NFCD_SRC ?=
NFCD_CORE ?= $(NFCD_SRC)/core/build/release/libnfc-core.a
NFCDEF_LIB ?=
CPPFLAGS += -Isrc $(shell $(PKG_CONFIG) --cflags $(PKGS))
ifneq ($(NFCD_SRC),)
CPPFLAGS += -I$(NFCD_SRC)/core/include
else
CPPFLAGS += $(shell $(PKG_CONFIG) --cflags nfcd-plugin)
endif
CFLAGS ?= -O2 -g
CFLAGS += -std=gnu11 -Wall -Wextra -Werror -fPIC
LDLIBS += $(shell $(PKG_CONFIG) --libs $(PKGS))
PLUGIN_DIR ?= /usr/lib/nfcd/plugins
SOURCES = src/linux_io.c src/linux_target.c src/linux_plugin.c
HEADERS = $(wildcard src/*.h)
.PHONY: all check clean install
all: build/linux.so
build:
	mkdir -p $@
build/linux.so: $(SOURCES) $(HEADERS) | build
	$(CC) $(CPPFLAGS) $(CFLAGS) -DNFC_PLUGIN_EXTERNAL -shared $(SOURCES) $(LDFLAGS) $(LDLIBS) -o $@
build/test-io: tests/test_io.c src/linux_io.c $(HEADERS) | build
	$(CC) $(CPPFLAGS) $(CFLAGS) tests/test_io.c src/linux_io.c $(LDFLAGS) $(LDLIBS) -o $@
check: build/test-io
	./build/test-io
ifneq ($(NFCD_SRC),)
build/core-target.o: $(NFCD_SRC)/core/src/nfc_target.c | build
	$(CC) $(CPPFLAGS) $(filter-out -Werror,$(CFLAGS)) -Wno-unused-parameter -DGLIB_VERSION_MIN_REQUIRED=GLIB_VERSION_2_38 -I$(NFCD_SRC)/core/src -c $< -o $@
build/test-target: tests/test_target.c src/linux_target.c src/linux_io.c build/core-target.o $(HEADERS) | build
	$(CC) $(CPPFLAGS) $(CFLAGS) tests/test_target.c src/linux_target.c src/linux_io.c build/core-target.o $(LDFLAGS) $(LDLIBS) -o $@
check: check-target
.PHONY: check-target
check-target: build/test-target
	./build/test-target
ifneq ($(NFCDEF_LIB),)
build/test-adapter: tests/test_adapter.c $(SOURCES) $(HEADERS) $(NFCD_CORE) | build
	$(CC) $(CPPFLAGS) $(CFLAGS) -I$(NFCD_SRC)/core/src tests/test_adapter.c src/linux_target.c src/linux_io.c -Wl,--whole-archive $(NFCD_CORE) -Wl,--no-whole-archive $(NFCDEF_LIB) $(LDFLAGS) $(LDLIBS) -ldl -o $@
check: check-adapter
.PHONY: check-adapter
check-adapter: build/test-adapter
	./build/test-adapter
endif
endif
install: all
	install -d $(DESTDIR)$(PLUGIN_DIR)
	install -m 0644 build/linux.so $(DESTDIR)$(PLUGIN_DIR)/linux.so
clean:
	$(RM) build/linux.so build/test-io build/test-target build/test-adapter build/core-target.o
