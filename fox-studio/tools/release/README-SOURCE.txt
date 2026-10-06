Fox Studio source snapshot
==========================

This is a fresh allowlisted source tree with no private repository history,
game assets, extracted profiles, community dictionaries or private pipeline.
SOURCE_MANIFEST.json lists the exact exported files and their SHA-256 hashes.
The workspace manifests omit inactive private dependencies and developer
targets that refer to files outside this snapshot.
Public integration tests and their authored synthetic fixtures are selected
individually. Private corpus tests and mixed-file private proof items are omitted;
the existing explicit render and packaged-tool test gates remain in place.

Build on Windows with the Rust MSVC toolchain and the Windows SDK installed.
The SDK resource compiler embeds the authored application icon. Native bundle
tests require an identity selected before compilation. Give your own source
build a clearly local identity. In PowerShell:

  $env:FOX_BUNDLE_BUILD_ID = 'local-source-build'

For a POSIX shell, use:

  export FOX_BUNDLE_BUILD_ID=local-source-build

From tools/rust, with that environment still set:

  cargo build --release --locked --no-default-features -p foxstudio -p foxcli -j 2
  cargo test --release --locked --workspace -j 2

The exported default feature set is public. The ordinary workspace test command
executes the authored native scheduler fixtures; do not ignore them to avoid
the identity requirement. The separate packaged-project concurrency case is
explicitly ignored and requires an additional named --include-ignored run.

The downloaded executables carry the audited distribution's common build ID.
Release scripts supply an audit-derived FOX_BUNDLE_BUILD_ID before compilation.
The local value above identifies your own build; it is not a released package
identity. Never reuse a published fox-tools.json with locally rebuilt binaries.
Production bundle version, identity and hash validation remains strict.
Running the editor with its sibling native fox executable does not require a
Python installation. CAPABILITIES.txt describes the public functionality and
the location generators that remain unfinished.

These commands build and test the standalone native source. They do not start
the game, install mods or validate GPU preview quality. Linux/platform and
visual acceptance evidence are separate release requirements.
