# Fish source build

This recipe retains the historical watch package's Fish 4.5.0 release, pinned
by commit. The Rust PCRE2 fork and registry crates are also pinned; checksums
in fish-crates.inc come from that release's Cargo.lock. All fetching happens
through BitBake; Cargo compiles offline with the frozen lockfile.

Fish, fish_indent, fish_key_reader, completions, functions, prompts, themes and
localized messages are installed. Embedded manual generation is disabled to
avoid adding documentation-generation dependencies to this watch build.
Fish is registered in /etc/shells without changing users' login shells.
