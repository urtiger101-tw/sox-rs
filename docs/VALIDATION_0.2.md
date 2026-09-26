# soundx 0.2 validation

Validated on Windows x64 with Rust 1.94.1 and Inno Setup 7.1.0.

## Executable size

All candidates include every existing codec, effect, MCP tool and Windows audio
control. Measurements are uncompressed executable bytes; no executable packer
or external codec backend is used.

| Build | Bytes |
| --- | ---: |
| v0.2, `opt-level=z`, CLI color and spelling suggestions enabled | 3,828,736 |
| v0.2, `opt-level=s`, reduced CLI features | 4,070,912 |
| Selected v0.2, `opt-level=z`, reduced CLI features | 3,803,136 |

Disabling optional CLI color and spelling suggestions saves 25,600 bytes
(0.67%) against the equivalent v0.2 feature baseline. Help, usage messages,
argument validation and all audio operations remain enabled. `z` was 267,776
bytes (6.58%) smaller than `s` in the measured build. Release settings retain
LTO, one codegen unit, symbol stripping and panic abort.

The earlier v0.1 executable was 3,391,488 bytes. Adding MCP, configuration-file
editing and Windows controls increases total size despite the above savings;
this release is not smaller than v0.1.

## Automated checks

- `cargo test --locked --all-targets`: **139 passed** on Windows.
- `cargo clippy --locked --all-targets -- -D warnings`: **PASS**.
- `cargo fmt --check` and `git diff --check`: **PASS**.
- Skill frontmatter validation: **PASS**.
- Real stdio tests cover initialization, tools/resources, Unicode file paths,
  WAV-to-FLAC processing, failed commands, job status, invalid recording format,
  malformed JSON and recovery after a request exceeds 1 MiB.
- Integration tests cover all four agents, comment preservation, conflicts,
  transaction rollback, config migration, preservation of user-owned files,
  an older installation's uninstaller, and partial removal failures.

## Native and installer checks

- Official MCP Python SDK **1.30.0**: initialization with protocol 2025-11-25,
  all eight tools, both resources, actual conversion and error reporting passed.
  The SDK is a test dependency only; it is not bundled with soundx.
- Core Audio endpoint read/no-op mute set/readback passed. Master volume and
  mute state were preserved.
- A silent soundx playback session was identified by its returned process id;
  its own volume/mute were changed, read back and restored successfully.
- Multi-file looping playback, graceful MCP stop, stdin EOF cleanup and forced
  MCP termination with Windows Job Object child cleanup passed.
- An actual Inno Setup installation into an isolated fixture passed with no
  agent tasks selected, then with all four selected. Five Skill files and four
  MCP entries used the installed executable. Existing settings survived.
- Actual fixture uninstall removed managed Skills and MCP entries, retained
  unrelated settings, and left the real user's PATH unchanged.

AGY generated the initial Windows module and independently reviewed the change.
Review findings were checked against the code and actual tests. Worker tools
were restricted; integration, corrections and validation were performed by the
main agent. No AGY worker was allowed to commit or publish.

## Verification boundaries

Agent configuration and Skill installation were tested in isolated profiles.
Discovery inside each agent's interactive UI was not tested. Physical microphone
capture was not performed in this validation; continuous WAV writing and early
stop behavior have automated coverage. Windows controls are explicitly
unsupported on other operating systems. The limits in
[SOX_COMPATIBILITY.md](SOX_COMPATIBILITY.md) continue to apply.
