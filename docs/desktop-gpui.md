# GPUI desktop migration

The target is a native GPUI desktop client on macOS, Linux and Windows. Android keeps the Expo application and native Canvas terminal. The daemon, provider adapters, terminal workers, wire protocol and stored conversations remain shared.

The existing Electron application stays available during migration. A native window, terminal demo or subset of features does not complete the migration. Switch the desktop entrypoints and packages only after the parity and deployment checks below pass.

## Boundaries

- `packages/desktop/native` owns the Rust application. It connects directly to daemon transports; it does not embed the web application or a JavaScript schema runtime.
- The daemon owns agents, PTYs, query replies and persisted data. Closing or upgrading a client never terminates user terminals.
- Native client capabilities describe implemented delivery semantics. Unknown optional wire fields remain accepted. See [protocol compatibility](protocol-compatibility.md).
- Pin GPUI and its platform crate to the same upstream revision and commit the Cargo lockfile. The framework baseline is Zed `v1.26.2`, commit `c4273271092b0287f7d181f616bd5171e0a2f70c`; framework upgrades need all three desktop builds.

## Development

Run `npm run dev:desktop:native` against the local daemon, or set `PASEO_HOME` to an isolated daemon home. An explicit `PASEO_ENDPOINT=ws://host:port/ws` selects a remote TCP endpoint. Credentials from a local home are never sent to an explicit remote endpoint. `PASEO_PASSWORD` supplies remote password authentication.

`cargo run -p paseo-desktop --no-default-features --bin paseo-native-inspect` exercises native discovery, authentication and paginated directory reads without opening a window. It prints counts and host identity, not credentials or transcripts.

On macOS with only Command Line Tools, pass `--features runtime-shaders` to Cargo. That uses GPUI's native Metal source compilation at application startup. Default distribution builds precompile Metal shaders and require Xcode's Metal tools.

The crates.io `gpui` 0.2.2 artifact predates the platform split in the current upstream source. Use the pinned Git pair rather than mixing the published crate with a newer platform backend. An early Linux check mapped a window but captured a black frame; a mapped native window alone is not rendering evidence.

## Acceptance

| Surface             | Required behavior                                                                                                                                   | Evidence |
| ------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- | -------- |
| Hosts               | Local discovery, socket/pipe, SSH, direct TLS, relay pairing, authentication, routed hosts, reconnect                                               | Pending  |
| Directory           | Projects, workspaces, live updates, filtering, archive, restore and remembered selection                                                            | Pending  |
| Agent navigation    | Built-in and terminal-owned agents, provider icons, activity, tab ownership, cached focused title and rename                                        | Pending  |
| Terminal            | Incremental replay, bounded history, resize ownership, Unicode/IME, selection/copy/paste, links, mouse/keyboard modes, images, splits and reconnect | Pending  |
| Conversations       | Complete paged history, streaming, Markdown/code/images, tools, permissions, composer drafts, attachments, provider controls and cancellation       | Pending  |
| Workspace tools     | Explorer, editor, search, git/diff/forge, scripts, browser and port/service integrations                                                            | Pending  |
| Product integration | Themes/settings, plugins, schedules/heartbeats, notifications, links, multi-window, updates and packaging                                           | Pending  |
| Platforms           | Native build and interaction on macOS, Linux and Windows; Android source/build behavior unchanged                                                   | Pending  |
| Performance         | Same-workload input/scroll latency, idle and busy CPU, memory by process, repeated navigation and lifecycle checks                                  | Pending  |

Use isolated test projects and homes. Do not type into live user agents. Compare the same retained workspace and workload before claiming a performance improvement.

The native keyboard encoder has focused regressions for application cursor mode, modified arrows, function keys, control punctuation and native IME ownership. These do not cover Kitty key events, operating-system IME interaction or native rendering; terminal acceptance remains pending until those paths are exercised.
