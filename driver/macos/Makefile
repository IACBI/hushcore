# HushCore Core Audio AudioServerPlugIn (macOS only). See README.md.

NAME       := HushCoreAudio
BUILD      := build
BUNDLE     := $(BUILD)/$(NAME).driver
BINARY     := $(BUNDLE)/Contents/MacOS/$(NAME)
PLIST      := $(BUNDLE)/Contents/Info.plist
TEST_BIN   := $(BUILD)/loopback_test

CC         ?= clang
ARCHS      := -arch arm64 -arch x86_64
MIN_OS     := -mmacosx-version-min=11.0
WARNINGS   := -Wall -Wextra -Werror
FRAMEWORKS := -framework CoreAudio -framework CoreFoundation

.PHONY: all test clean

all: $(BINARY) $(PLIST)
	codesign --force --sign - $(BUNDLE)

$(BINARY): src/HushCoreAudio.c
	mkdir -p $(dir $@)
	$(CC) -std=gnu11 $(ARCHS) $(MIN_OS) -bundle -O2 $(WARNINGS) $(FRAMEWORKS) -o $@ $<

$(PLIST): Info.plist
	mkdir -p $(dir $@)
	cp $< $@

$(TEST_BIN): tests/loopback_test.c
	mkdir -p $(dir $@)
	$(CC) -std=gnu11 $(ARCHS) $(MIN_OS) -O2 $(WARNINGS) $(FRAMEWORKS) -o $@ $<

test: $(TEST_BIN)
	./$(TEST_BIN)

clean:
	rm -rf $(BUILD)
