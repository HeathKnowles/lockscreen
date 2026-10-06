# PLAN.md — `phoned`: KDE Connect–style remote with full-system control

Status: **v2.1 — approved, P0 next.** Decision history in §11.

## 1. Vision & positioning

A phone app + a single Linux daemon that lets the phone control the desktop:
KDE Connect's feature set, plus deeper, policy-bound system control.

| Capability | KDE Connect | `phoned` |
|---|---|---|
| Presence / auto-lock | partial, network-based | **BLE presence, instant lock on link loss, works offline (built & deployed)** |
| Notifications, clipboard, files, media, remote input, battery, find-device | yes | yes (P4/P5) |
| Run predefined commands | config list | allowlist **+ permission tiers + audit log + confirm-token** |
| Power: suspend / reboot / off | partial | yes, via `org.freedesktop.login1` |
| Telemetry + process list / kill | no | yes |
| App launcher | no | yes (XDG `.desktop` scan) |
| Screenshots | no | yes (xdg-desktop-portal, `grim` fallback) |
| Per-device permission tiers | implicit | **Observer / Controller / Admin** |
| Stack | C++/Qt, KDE-centric | Rust daemon, gRPC/protobuf, compositor-agnostic APIs |

**Pitch:** the phone becomes a *policy-bound admin token* for the machine —
daily controls just work, dangerous operations are explicit, logged and revocable,
and the lock function never depends on the network.

## 2. Settled decisions

| Area | Decision |
|---|---|
| Language | Rust for the daemon and presence (existing code, zbus, cached deps). Kotlin + `grpc-java` (OkHttp) on Android. |
| Transport | gRPC over HTTP/2: `tonic` + `prost` + `tonic-build` (system `protoc`). Streaming for status, unary with 1 s deadline for actions. |
| Wire format | Protocol Buffers, **one `.proto` per feature**, namespace `phoned/v1/`. |
| Security | TLS 1.3 (`rustls`, self-signed cert pinned by the phone during pairing) + `Control.Pair`/`Control.Auth` session token enforced by a tonic interceptor. |
| Process layout | **One systemd user unit, one main process.** `phoned` spawns and supervises `lockscreen` as a child (the BLE fallback). |
| Responsibility split | `lockscreen` = authority for *automatic* lock (presence). `phoned` = gRPC server, status streams, and lock/unlock only on explicit RPC. Exactly one auto-lock authority. |
| Command policy | Allowlist (`commands.toml`, argv exec — never a shell string) by default; **opt-in raw shell** per device, confirm-token on every call. |
| Target desktops | **Hyprland / wlroots first → KDE Plasma → COSMIC**, behind an `Integration` trait. |
| Scope order | P0 skeleton → P1 MVP link+lock → P2 presence upgrade → **P3 control core** → P4 daily-use → P5 sync/input → P6 hardening. |
| Defaults | listen TCP `43152` on the LAN (`--listen`), mDNS discovery deferred to P2, storage `~/.config/phoned/`, Android minSdk 26 (background advertising is service-UUID-only from Oreo — matches our matcher). |

## 3. Architecture

```text
Android app (Kotlin, foreground service)
  ├─ BLE advertiser ── service UUID ──► laptop scans (BlueZ) ──┐
  └─ gRPC client  ◄── HTTP/2 + TLS ──►  phoned (Rust, tokio)  │
                                                               ▼
                                   presence module ◄── features: lock, power, media,
                                          │                     notify, clipboard, files…
                                          └── loginctl / zbus / wpctl / portal / uinput
```

Two **decoupled** paths by design:

| | BLE presence | gRPC channel |
|---|---|---|
| Purpose | automatic lock (must never fail) | commands, status, sync |
| Dependency | none — works with the app absent | phone app alive |
| Latency | link-supervision timeout → ~0.5 s | 1–10 ms on LAN |

Lock **must not** depend on TCP: Android Doze kills sockets, BLE keeps working.

**Process model:** one unit, one main PID (`phoned`), `lockscreen` as a supervised
child restarted on crash. On daemon restart the presence path re-arms in ~1 s
(connected phone → armed), so lock coverage gaps are negligible.

## 4. Control model (the "more control" part)

Three tiers per paired device, stored in `~/.config/phoned/devices.toml`,
toggled from the phone UI:

| Tier | Allowed RPCs |
|---|---|
| **Observer** | status / telemetry streams, notification mirror |
| **Controller** (default) | lock/unlock, media, volume, clipboard, files, app launch, screenshot |
| **Admin** | power, process kill, `cmd.Run`, `cmd.RunShell`, input injection |

Guardrails for anything destructive:

- **Confirm-token flow**: `RequestAction` → phone shows a dialog →
  `ConfirmAction(token)` within 10 s → execute. Token binds args + device.
- **Audit log**: every privileged call (device id, RPC, args, result) →
  journal **and** `~/.config/phoned/audit.log`.
- **Allowlisted commands** — `~/.config/phoned/commands.toml`:

  ```toml
  [[command]]
  id = "backup-home"
  argv = ["rsync", "-a", "/home/user/", "/mnt/backup/home/"]  # argv exec, never a shell string
  timeout = "10m"

  [shell]
  enabled = false              # global off by default
  devices = ["oneplus-nord5"]  # only these devices may call RunShell
  ```

  `cmd.Run` (allowlist) needs no confirm-token — it is pre-approved by config.
  `cmd.RunShell` only for devices listed under `shell.devices`, **always**
  confirm-token, runs in `$HOME` with no TTY, output streamed and capped
  (64 KiB), 30 s default timeout, every call audited.
- **Revoke**: `phoned revoke <device-id>` drops the pin and kills live sessions.

## 5. Protocol surface

```text
frame := HTTP/2 data message carrying one protobuf Envelope
Envelope { proto_version, seq, ts_ms, oneof body }
```

```text
proto/phoned/v1/
  control.proto     Pair, Auth, Ping, Capabilities, Revoke
  presence.proto    WatchPhone (battery/screen/charging), WatchLink (RSSI/connected/room)
  telemetry.proto   WatchSystem (cpu/ram/disk/net), ListProcesses, KillProcess
  power.proto       Suspend, Hibernate, Reboot, PowerOff, Logout, WatchPowerEvents
  apps.proto        ListApps, LaunchApp
  media.proto       Play, Pause, Next, Prev, SetVolume, WatchNowPlaying
  notify.proto      PushNotification, ListRecent
  clipboard.proto   Get, Set
  files.proto       Send (client-streaming), Receive, Progress
  input.proto       Pointer, Key, Text
  screenshot.proto  Capture
  cmd.proto         ListAllowed, Run, RunShell, WatchOutput
```

Handshake: `Hello` (version, device id, capabilities) → `Pair` (6-digit code →
device identity key pinned in `devices.toml`) → `Auth` (signed challenge →
session token) → all further calls carry the token in `metadata`
`authorization`; the interceptor rejects unauthenticated calls before handlers.
Liveness via HTTP/2 pings (`http2_keep_alive_interval` in tonic, equivalent in
grpc-java) — no hand-rolled keepalive messages.

Honest tradeoff accepted: ~1–2 KB extra per message and codegen on both sides,
in exchange for streaming, deadlines, interceptors and mature Android tooling.
Irrelevant to the lock path, which is BLE-only.

## 6. Integration matrix (drives the `Integration` trait)

| Feature | Hyprland / wlroots **(first)** | KDE Plasma **(2nd)** | COSMIC **(3rd)** |
|---|---|---|---|
| Lock | `--lock-cmd` → `hyprlock`/`swaylock`; do **not** assume `loginctl lock-session` is handled | `loginctl lock-session` ✓ verified on this box | its locker via login1 |
| Clipboard | `wl-copy` / `wl-paste` (history kept by `phoned` itself) | same (Plasma implements the core data-device protocol) | same |
| Screenshot | `xdg-desktop-portal-hyprland`, fallback `grim` | portal (Spectacle) | portal |
| Volume / media | `wpctl` (PipeWire) + MPRIS — compositor-independent | same | same |
| Power | `org.freedesktop.login1` — compositor-independent | same | same |
| Notifications → desktop | plain `Notify` (mako/dunst) | plasma notification daemon | cosmic daemon |
| Notifications → phone | risk item (§10) | KDE private API later | later |
| Remote input | `/dev/uinput` (runtime probe; feature flags itself off) | same | same |
| Window control (future) | `hyprctl` IPC | KWin D-Bus | COSMIC compositor API |

Design consequence: every feature goes through an `Integration` trait with a
backend chosen at startup; the default path is compositor-agnostic
(wl-clipboard, wpctl, login1, portal, MPRIS), so Hyprland and KDE share one
backend — only lock, screenshot fallback and future window control are
per-compositor.

## 7. Phases

| Phase | Deliverable | Acceptance criteria |
|---|---|---|
| **P0** | Cargo workspace, `control.proto` + `presence.proto`, codegen (`build.rs` → tonic-build), presence modules moved from `lockscreen`, global `--dry-run`, `DECISIONS.md` | `cargo fmt --check && cargo clippy --all-targets && cargo test` green; `phoned --help`; fallback child starts and stops cleanly |
| **P1** | Pairing + TLS pinned cert + session auth + `WatchPhone`/`WatchLink` streams + `Lock`/`Unlock` | fake-phone integration test: pair → authenticate → `Lock` fires dry-run lock; unauthenticated RPC rejected; journal shows authenticated session |
| **P2** | Phone advertises fixed 128-bit service UUID → `-m service:<uuid>` → real RSSI → **room-exit lock**, keeping `-D 0s` for link loss | `--list` calibration walk across rooms; weak-lock fires at the calibrated threshold |
| **P3** | **Control core**: power, telemetry, processes, app launcher, Observer/Controller/Admin tiers, confirm-token, audit log, `cmd.Run` allowlist + opt-in `RunShell` | phone suspends/relaunches/kills with confirm flow; audit line written; Observer tier rejected; `RunShell` blocked for a non-listed device |
| **P4** | Media, notifications (phone→desktop solid; desktop→phone mirror behind a research flag), screenshot | daily-driver usable on Hyprland and KDE |
| **P5** | Clipboard, files, input | all behind Controller/Admin tiers; input degrades cleanly when uinput is unavailable |
| **P6** | Hardening: battery/reconnect tuning, policy UI, mDNS discovery, optional QUIC evaluation, security pass | documented revoke runbook; reconnect storm test passes |

Rationale: control core (P3) before daily-use polish because "phone controls
the machine" is the point of the project. Swapping P3↔P4 is a one-line change
if priorities move.

## 8. Repo layout

```text
proto/phoned/v1/*.proto          one file per feature (§5)
src/bin/phoned.rs                main: spawn fallback child, tonic server, shutdown
src/bin/lockscreen.rs            existing binary, behavior unchanged
src/presence/{scanner,device,watch,list,log}.rs   moved from the lockscreen crate
src/rpc/{server,auth,tls}.rs     tonic server, session interceptor, cert handling
src/features/{lock,power,telemetry,apps,media,notify,clipboard,files,input,cmd}.rs
src/integration/{mod,hyprland,kde,generic}.rs      compositor backends (§6)
src/fallback.rs                  spawn + supervise the lockscreen child
build.rs                         tonic-build / prost-build (system protoc 3.19.6)
contrib/phoned.service           single user unit (replaces the current unit's ExecStart)
docs/e2e.md                      manual end-to-end checklist per phase
DECISIONS.md                     short record of architectural calls
PLAN.md                          this document
```

Environment facts this plan relies on: Fedora, rustc/cargo 1.97.1, **no root**
(pexec only), zbus (no libdbus), `protoc` 3.19.6 installed, `tokio`/`rustls`/
`prost`/`bytes`/`ring` already in the cargo cache, crates.io reachable,
Android SDK 35–37 + JDK 25 (Gradle via wrapper, services.gradle.org reachable).

## 9. Testing strategy

- **Unit**: framing/auth/token state machines; feature handlers tested against
  an `Integration` trait with injected fakes (no live D-Bus in unit tests).
- **Global `--dry-run`**: every privileged handler logs its intent instead of
  acting — the same trick that validated the lockscreen work.
- **Fake-phone gRPC client**: Rust test that pairs, authenticates, drives each
  RPC in dry-run and asserts audit entries.
- **Golden protobuf vectors**: Rust-generated bytes asserted by Kotlin tests
  and vice versa — catches codegen drift before E2E.
- **Manual E2E checklist** per phase in `docs/e2e.md`
  (`--list`, `-n`, `journalctl --user -u phoned -f`).

## 10. Risks & open items

1. **Desktop→phone notification mirroring** — no portable subscribe API.
   Options: take over the `org.freedesktop.Notifications` name and re-emit, or
   compositor-specific hooks. P4 ships phone→desktop first; mirror stays behind
   a research flag.
2. **Lock semantics differ per compositor** — `--lock-cmd` already exists;
   add a `lock-backend` config with per-compositor defaults and a `--check-lock`
   probe verifying the configured command actually locks.
3. **polkit for power actions** — may prompt/deny; P3 verifies login1
   permissions for an active local session and documents the fallback.
4. **uinput availability** — needs the `input` group; probe at startup and
   disable `input.proto` if unavailable.
5. **Android Doze** — streams drop; the lock path is BLE-only so the important
   function never depends on the socket.
6. **The phone is an admin token** — mitigated by tiers, confirm-token, audit
   log and revoke; state this loudly in the app UI.
7. **Dev box vs target**: this machine runs KDE today while Hyprland is the
   design target — build behind `Integration`, verify on Hyprland when available.
8. **Carry-over experiments**: connected-phone RSSI is unavailable (controller
   returns 0 for BR/EDR links — verified), so P2's app-side BLE advertising is
   the only route to room-level detection.

## 11. Change log

| Version | Change |
|---|---|
| v0 | Original BLE walk-away lock plan: TCP vs QUIC vs BLE discussion; ring buffers ruled out as cross-device transport (internal-only); presence lock already built. |
| v1 | Decisions: gRPC over HTTP/2 (user), single daemon (user), protobuf, MVP link+lock first. |
| v1.1 | Protos split per feature; TLS + pinned cert; `lockscreen` kept **in the same unit** as supervised BLE fallback. |
| v2 | Repositioned as "KDE Connect + full system control": permission tiers, confirm-token, audit, power/telemetry/process/app/screenshot surfaces. |
| v2.1 | Target compositors: Hyprland first → KDE → COSMIC; P3 = control core; shell = allowlist + opt-in per device. **← current** |
