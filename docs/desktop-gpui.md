# GPUI desktop migration

The target is a native GPUI desktop client on macOS, Linux and Windows. Android keeps the Expo application and native Canvas terminal. The daemon, provider adapters, terminal workers, wire protocol and stored conversations remain shared.

The existing Electron application stays available during migration. A native window, terminal demo or subset of features does not complete the migration. Switch the desktop entrypoints and packages only after the parity and deployment checks below pass.

The existing desktop UI is the visual specification. Preserve its layout, typography, icons, colors, spacing, control geometry and interaction states pixel for pixel. Use the authored tokens in `packages/app/src/styles/theme.ts` and the existing components as references. Native migration does not authorize a redesign.

## Boundaries

- `packages/desktop/native` owns the Rust application. It connects directly to daemon transports; it does not embed the web application or a JavaScript schema runtime.
- The daemon owns agents, PTYs, query replies and persisted data. Closing or upgrading a client never terminates user terminals.
- Native client capabilities describe implemented delivery semantics. Unknown optional wire fields remain accepted. See [protocol compatibility](protocol-compatibility.md).
- Pin GPUI and its platform crate to the same upstream revision and commit the Cargo lockfile. The framework baseline is Zed `v1.26.2`, commit `c4273271092b0287f7d181f616bd5171e0a2f70c`; framework upgrades need all three desktop builds.

## Development

Run `npm run dev:desktop:native` against the local daemon, or set `PASEO_HOME` to an isolated daemon home. An explicit `PASEO_ENDPOINT=ws://host:port/ws` selects a remote TCP endpoint. Credentials from a local home are never sent to an explicit remote endpoint. `PASEO_PASSWORD` supplies remote password authentication.

`cargo run -p paseo-desktop --no-default-features --bin paseo-native-inspect` exercises native discovery, authentication and paginated directory reads without opening a window. It prints counts and host identity, not credentials or transcripts.

Run `npm run sync:desktop:native-ui` after changing authored theme tokens, provider icons or window controls. The generated native assets reuse the app's default dark tokens, identity palette, built-in provider vectors, installed Lucide RN geometry and authored window-control component. `npm run check:desktop:native` rejects stale output. Native theme selection and custom appearance settings still require migration.

On macOS with only Command Line Tools, pass `--features runtime-shaders` to Cargo. That uses GPUI's native Metal source compilation at application startup. Default distribution builds precompile Metal shaders and require Xcode's Metal tools.

The crates.io `gpui` 0.2.2 artifact predates the platform split in the current upstream source. Use the pinned Git pair rather than mixing the published crate with a newer platform backend. A mapped native window alone is not rendering evidence. The pinned baseline has now drawn the directory and terminal in isolated Linux QA; hardware rendering and the other desktop platforms still require interaction checks.

## Acceptance

| Surface             | Required behavior                                                                                                                                   | Evidence |
| ------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- | -------- |
| Hosts               | Local discovery, socket/pipe, SSH, direct TLS, relay pairing, authentication, routed hosts, reconnect                                               | Pending  |
| Directory           | Projects, workspaces, live updates, filtering, archive, restore and remembered selection                                                            | Pending  |
| Agent navigation    | Built-in and terminal-owned agents, provider icons, activity, tab ownership, cached focused title and rename                                        | Pending  |
| Terminal            | Incremental replay, bounded history, resize ownership, Unicode/IME, selection/copy/paste, links, mouse/keyboard modes, images, splits and reconnect | Pending  |
| Conversations       | Complete paged history, streaming, Markdown/code/images, tools, permissions, composer drafts, attachments, provider controls and cancellation       | Pending  |
| Workspace tools     | Explorer, editor, search, git/diff/forge, scripts, browser and port/service integrations                                                            | Pending  |
| Product integration | Themes/settings/localization, plugins, schedules/heartbeats, notifications, links, multi-window, updates and packaging                              | Pending  |
| Visual parity       | Matched reference/native captures per desktop OS, viewport, scale, theme, font setting and state; pixel differences reviewed and resolved           | Pending  |
| Platforms           | Native build and interaction on macOS, Linux and Windows; Android source/build behavior unchanged                                                   | Pending  |
| Performance         | Same-workload input/scroll latency, idle and busy CPU, memory by process, repeated navigation and lifecycle checks                                  | Pending  |

Linux interaction evidence is in [the native terminal capture](qa-evidence/gpui-native-linux/validation.json) and its adjacent screenshot. The fixture received keyboard input and SGR wheel reports; closing and reopening the client retained the same daemon-owned terminal and its input. This uses software rendering and does not establish hardware performance or full terminal parity.

Agent navigation evidence is in [the native Agent capture](qa-evidence/gpui-native-agents/validation.json). An isolated fake-provider fixture exercises built-in history and live catch-up alongside a Pi hook registered terminal. The later [composer and visual comparison](qa-evidence/gpui-native-composer/validation.json) exercises OS text entry, submission, cancellation and independent drafts across Agent navigation. Provider vectors now replace the prototype text chips. Its pixel comparison is explicitly **not passed**: font rendering, controls and states remain incomplete. Rich content, permissions, durable drafts and full Agent-tab lifecycle acceptance remain pending.

[The Linux font and window comparison](qa-evidence/gpui-native-window/validation.json) uses an actual Electron renderer as the reference. It verifies default font selection, the history-start slot, sidebar collapse, frameless window controls, movement, resizing and retained drafts with a real window manager. Closing the client retains the isolated daemon's agents and history. Pixel acceptance remains **not passed**; the other desktop platforms, appearance settings, controls and first-paint behavior still need evidence.

[The native provider-control check](qa-evidence/gpui-native-controls/validation.json) confirms model, thinking and mode changes by reading the isolated daemon. [The attachment check](qa-evidence/gpui-native-attachments/validation.json) adds real X11 file drops, binary upload and image-byte verification at the fake provider, navigation retention, preview dismissal and visible size/picker failures with retry or removal. The matched Electron comparisons remain **not passed**. Successful OS file picking, full image interaction, file preview/download, sent attachment rendering, forge/plugin menus, voice controls, complete model browsing and responsive control density still need migration.

Use isolated test projects and homes. Do not type into live user agents. Compare the same retained workspace and workload before claiming a performance improvement.

Capture the reference and native client against the same isolated daemon and data. Cover empty, idle, running, attention, selected, hovered, pressed, disabled and open-menu states. Fix the viewport, device scale, theme, fonts, sidebar widths and scroll position. Keep image diffs as evidence. Mask only recorded nondeterministic regions such as a timestamp or spinner angle; do not hide geometry or text differences behind a broad similarity threshold. Framework text rasterization differences remain explicit unresolved evidence until checked on that desktop OS.

The native keyboard encoder has focused regressions for application cursor mode, modified arrows, function keys, control punctuation and native IME ownership. These do not cover Kitty key events, operating-system IME interaction or native rendering; terminal acceptance remains pending until those paths are exercised.
