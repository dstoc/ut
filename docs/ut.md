# `ut` — System & Architecture

`ut` is a Sway-first dictation CLI for Wayland. You tap a key to start recording,
tap again to stop; `ut` captures microphone audio, sends it to an
OpenAI-compatible chat-completions endpoint with `input_audio`, and pastes the
returned text into the focused window. An optional GPU status overlay shows live
recording and processing feedback.

This document explains how the system fits together: its runtime model, the shape
of a dictation session, how concurrency and state are coordinated, and the design
decisions and invariants worth knowing before changing anything. It supersedes
the design proposals that previously lived alongside it.

## What problem the design solves

A dictation session is longer-lived than a single command. The user presses a key
to start recording and presses again — a *separate* process invocation — to stop
and transcribe. So `ut` cannot be a simple run-to-completion CLI: the first
invocation has to keep running (recording, transcribing, pasting) while later
invocations reach back to it to control it.

Everything in the architecture follows from that: there is one long-lived **owner
process** per machine, and other invocations are thin **clients** that talk to it.
The rest is the machinery to start an owner reliably, recover when one crashes,
drive the session through its phases, and tear it down cleanly.

## The owner/client model

At most one owner runs at a time, identified by three artifacts in a per-user
runtime directory: a **lock file** holding the owner's PID, a **control socket**
the owner listens on, and a **state file** holding the session's current phase and
captured context.

When you invoke `ut`:

- If it needs to *control* an existing session (`stop`, `abort`, `status`), it
  connects to the control socket and sends a one-line command, reading one line
  back.
- If it needs to *start* a session, it acquires the lock, binds the socket, and
  becomes the owner — staying alive until the session finishes.
- `toggle` (the default, two-tap behavior) tries to control first; if no owner
  answers, it becomes one. So the first tap starts recording and the second tap
  reaches the owner and stops it.

**Crash recovery** is built into lock acquisition. A crashed owner leaves stale
artifacts behind. The next invocation reads the recorded PID and probes whether
that process is still alive; if it is, the new invocation defers, and if it isn't,
it cleans up the stale runtime and claims ownership. The same liveness probe lets
a client distinguish "no owner exists, I should start one" from "the owner is
alive but the socket call failed, surface the error."

## Anatomy of a session

Once a process becomes the owner, it runs a single dictation session as an
explicit state machine through four phases — **Recording → Processing → Pasting →
Idle** — with abort possible at every step.

1. **Recording.** Microphone capture starts and the overlay (if enabled) shows a
   live audio-reactive indicator. The session waits until the user stops it, an
   abort arrives, or a configured maximum duration elapses.

2. **Processing.** The window context is captured again, capture finishes into an
   audio buffer, optional silence trimming runs, and the buffer is normalized to
   the fixed transcription format. Empty audio short-circuits straight back to
   idle.

3. **Transcribing.** A prompt is selected (per-window rules can override it), and
   the audio is sent to the model endpoint. This request runs on a worker so it
   can be cancelled the instant an abort arrives, rather than blocking until the
   network call returns.

4. **Pasting.** The window context is captured a third time. If focus is still on
   the window where recording began, the text is injected; if focus moved, the
   text is only copied to the clipboard and the user is notified — `ut` never
   types into the wrong window. Paste failures are reported and leave the session
   idle.

Every abort, empty-audio, and cancellation path converges on one teardown helper
(dismiss the overlay, return to idle), so there is a single place that defines
"end this session cleanly."

### Capturing window context three times

Focus safety depends on knowing *which* window the dictation was meant for.
Context (compositor, app id, window class/title, PID, executable, working
directory, and the compositor's container id) is captured at recording start,
again at stop, and again just before paste. The start-vs-pre-paste comparison of
the container id is what decides whether auto-paste is safe. Context is gathered
by asking the Sway compositor for the focused node and enriching it from `/proc`;
if the compositor can't be reached, capture degrades gracefully to an empty
context rather than failing the session.

## Concurrency and the single source of truth

An owner session runs several threads at once: the session state machine itself,
a control server accepting client commands, a transcription worker, and — when
the overlay is active — a render thread and an audio-pump thread. They need to
agree on one thing above all: what phase the session is in.

That agreement is enforced by a single shared **control state** object built
around one mutex and a condition variable. It holds the in-memory phase and the
stop/abort/shutdown request flags, and it is the **only writer of the persisted
state file while a session is live**. Phase transitions and metadata updates all
flow through it, serialized by its lock, so the on-disk phase never depends on a
race between threads. The free-standing state helpers exist only for the
bootstrap and cleanup paths that run before any session — and therefore any
control state — exists.

Client commands arriving on the socket (`stop`, `abort`) set request flags on the
control state and wake the waiting session thread through the condition variable.
The waiting loops poll on a short timeout as well, which keeps the design simple;
the cadence is fine for a human-driven tool. Abort always wins over a pending
stop.

The transcription worker is the one place that deliberately steps outside the
control state's mutex: it owns an async runtime and races the HTTP request against
a cancellation signal, so an abort during a slow model call returns immediately
instead of waiting for the network.

## Audio path

There are two consumers of the microphone, and the architecture keeps them
separate so neither degrades the other:

- **Transcription** needs a clean, complete recording. Capture accumulates the
  full signal, then down-mixes and resamples it to a fixed mono 16 kHz format at
  the end. That format is not configurable — the models expect it, and the rest
  of the audio code assumes it.

- **Visualization** needs a cheap, live signal for the overlay. From inside the
  capture callback (where the samples are already in hand) `ut` computes a compact
  per-callback snapshot — loudness envelope, a transient accent, a few frequency
  bands, and a coarse waveform — and forwards it to the overlay on a bounded
  channel that *drops on backpressure*. Visualization can never stall capture.

The signal-processing math for that snapshot lives in one shared place used by
both the producer (capture) and the consumer (overlay), so the two halves of the
pipeline can't drift apart and the compression curve exists exactly once.

When built without microphone support, a stub capture implementation preserves the
same interface but records nothing — enough to compile and exercise the rest of
the pipeline without audio hardware.

## Transcription client

The client talks to any OpenAI-compatible chat-completions endpoint; it is
deliberately model-neutral (its naming reflects the task, "dictation," not any
particular model). A request carries the selected prompt as a system message and
the recording as a base64 WAV `input_audio` part, at zero temperature. The
response parser accepts both plain-string and structured content. Authentication,
if configured, resolves the API key *at request time* (so a rotated environment
variable is honored without restarting), from a single resolution routine shared
with config validation. The client sits behind a small trait so tests can
substitute fakes — for example, to prove that an abort during transcription
returns promptly.

## Paste and focus safety

Pasting has two strategies — drive a paste shortcut after putting the text on the
clipboard, or type the text directly — and a safety policy layered on top. The
policy is the important part: if the focused window changed between recording and
paste, `ut` falls back to copy-only and notifies the user rather than injecting
keystrokes somewhere unintended. Per-window rules can also override the paste
shortcut (translated into the underlying key-injection tool's modifier/key
sequence). Clipboard restoration is best-effort: the previous clipboard contents
are saved and put back after a short delay, and any failure along the way notifies
rather than aborts.

`ut` leans on external Wayland helpers for clipboard, key injection,
compositor queries, and notifications. Missing helpers degrade to clear messages
instead of crashing the session.

## The status overlay

The overlay is optional at both compile time and runtime. It is a borderless,
always-on-top, transparent Wayland layer-shell surface that renders a shader keyed
to the session phase: an audio-reactive orb while recording, a deterministic
animation while processing, and a fade-out at the end.

Its defining architectural property is that it is a pure **observer**. It never
owns or mutates session state; the session pushes phase changes and audio
snapshots to it, and it renders them. This is enforced by a facade: the session
loop only ever talks to one overlay-session type, which is either the real
Wayland/GPU-backed implementation or a no-op stub selected at build time. As a
result the session logic is identical whether or not the overlay is compiled in,
and a failure to start the overlay at runtime is non-fatal — dictation continues
without it.

Internally the real overlay runs the GPU render loop on its own thread and a small
pump thread that forwards audio snapshots to it, so neither competes with capture
or transcription.

## Configuration

Configuration is a single optional TOML file; its absence yields working defaults.
It covers recording limits and trimming, the model endpoint and auth, the paste
method and focus-change behavior, the overlay's size/position/fade, a set of named
prompts, and a list of per-window rules. Rules match on window identity (app id,
class, or a title substring) and can select a named prompt and a custom paste
shortcut. Validation rejects nonsensical values (empty model, non-positive
timeouts/dimensions, out-of-range positions, a non-http(s) URL, or a rule
referencing an undefined prompt), and the same validation backs the `health`
command, which additionally checks for the required helper commands and that the
microphone can be opened.

## Build-time variation

Two optional features, both on by default, gate the two heaviest dependency sets:
microphone capture (which pulls in ALSA on Linux) and the GPU overlay. Each can be
dropped independently. Turning either off swaps in a stub that keeps the same
interface, so the core flow always compiles and the only difference is capability.
The full feature matrix is what keeps the abstraction honest — the codebase
compiles and is testable with or without audio hardware and with or without a GPU.

A naming convention runs through this: the short internal names (`ui` feature,
`overlay` engine) describe the implementation, while the names a *user* sees — the
`status_ui` config section and the shader file — keep the product term "status."
Internal names were free to shorten; user-facing names were not, because changing
them would break existing config files.

## Invariants & gotchas

These are the constraints most likely to be violated by a well-meaning change:

- **One writer for session phase.** While a session is live, only the shared
  control state persists the phase, serialized by its mutex. Don't add in-session
  state writes elsewhere; the free state helpers are for the no-owner
  bootstrap/cleanup paths only.

- **The "legacy" visualization fields are load-bearing.** The visualization
  snapshot carries both newer envelope/band features and older waveform/rms/peak
  values. The older ones are *not* dead: the shader reads them every frame, the
  voice-gate consumes them, and the recording path deliberately recomputes them
  from the newer features before handing them to the GPU. Slimming the snapshot
  means rewriting the shader in the same change — it is a reactivity change, not a
  dead-field cleanup. (A proposal to remove them was investigated and rejected for
  exactly this reason.)

- **Capture is always mono 16 kHz.** There is no configurable sample rate or
  channel count; trimming and WAV encoding assume this format.

- **Visualization must never block capture.** Snapshots are sent on a bounded
  channel and dropped under backpressure.

- **Overlay failure is non-fatal.** No overlay (by build or by runtime failure)
  must leave dictation fully working.

- **API-key resolution is call-time and single-sourced**, so a rotated
  environment variable is honored without a restart.

### Deliberate extension points

A couple of things look over-built for the present and are intentionally so: the
focus-change action is modeled as an enum with a single variant today, left
extensible for a future "type instead of copy" behavior, and the persisted state
file carries a version field so its shape can evolve. Treat the single-armed match
on the former as intentional, not incomplete.

## Build & verification

```
cargo build                                                # default: capture + overlay (needs ALSA)
cargo build --no-default-features --features audio-capture # no overlay / no graphics deps
cargo build --no-default-features --features ui            # overlay only, stub capture
cargo test                                                 # default features
```

Keep all feature combinations green: no-default, ui-only, audio-capture-only
(needs ALSA), and the default both-features build — the last is the only one where
dead-code detection sees every shared DSP helper as live. Runtime helpers
required: `swaymsg`, `wl-copy`, `wl-paste`, `wtype`, `notify-send`.
