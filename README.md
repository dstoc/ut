# `ut`

`ut` is a Sway-first dictation CLI for Wayland.

The MVP uses a two-tap toggle flow:

- first `ut` invocation (or `ut start`) starts recording
- second `ut` invocation (or `ut stop`) stops recording, processes audio with the specified LLM, and pastes the result
- `ut abort` cancels the active session

## Getting Started

Install `ut`:

```bash
cargo install --locked --git https://github.com/dstoc/ut
```

Create `~/.config/ut/config.toml` and specify model config (see below).

Confirm dependencies, audio input, and config are valid:
```bash
ut health
```

Add Sway bindings, e.g.:

```conf
bindsym $mod+equal exec ut start
bindsym --release $mod+equal exec ut stop
```

## Build Requirements

Rust:

- stable Rust toolchain
- Cargo

System libraries:

- ALSA development package
- `pkg-config`

On Debian or Ubuntu this typically means:

```bash
sudo apt install build-essential pkg-config libasound2-dev
```

## Runtime Requirements

`ut` expects these commands at runtime:

- `swaymsg`
- `wl-copy`
- `wl-paste`
- `wtype`
- `notify-send`

On Debian or Ubuntu:

```bash
sudo apt install sway wl-clipboard wtype libnotify-bin
```

## Build

```bash
cargo build
```

The default build enables two features:

- `audio-capture` — microphone capture via `cpal` (requires the ALSA development headers)
- `ui` — the status overlay, drawn with `wgpu` on a Wayland layer-shell surface

To build without the overlay and its graphics dependencies:

```bash
cargo build --no-default-features --features audio-capture
```

If the build fails with an `alsa.pc` or `alsa-sys` error, install the ALSA development package and make sure `pkg-config` can find it.

Optional install without the overlay:

```bash
cargo install --locked --git https://github.com/dstoc/ut --no-default-features --features audio-capture
```

## Model

The current default model settings target:

- URL: `http://127.0.0.1:11434/v1`
- model: `unsloth/gemma-4-E2B-it-GGUF:Q4_K_XL`
- optional auth: set `model.api_key` directly, or `model.api_key_env` to read a bearer token from an environment variable at request time

The request uses the OpenAI-compatible chat completions API with `input_audio`.

## Usage

```bash
ut
ut toggle
ut start
ut stop
ut abort
ut status
ut health
```

`ut` without a subcommand is equivalent to `ut toggle`.

## Configuration

Config is loaded from:

- `$XDG_CONFIG_HOME/ut/config.toml`
- or `~/.config/ut/config.toml`

Defaults:

```toml
[recording]
max_seconds = 29
trim_silence = true
trim_padding_ms = 500

[model]
url = "http://127.0.0.1:11434/v1"
model = "unsloth/gemma-4-E2B-it-GGUF:Q4_K_XL"
timeout_seconds = 60

[paste]
method = "clipboard"
restore_clipboard = true
restore_delay_ms = 100
on_focus_changed = "copy"

[status_ui]
enabled = true
width = 200
height = 200
x = 0.5
y = 0.8
fade_out_ms = 350
```

Optional model auth:

```toml
[model]
api_key = "sk-..."
# or:
api_key_env = "OPENAI_API_KEY"
```

If both are set, `api_key` wins. When a key is resolved, `ut` sends it as `Authorization: Bearer <key>`.

Health check:

```bash
ut health
```

This checks that the config parses and passes basic validation, required helper commands are present in `PATH`, and an input audio stream can be opened.

App rule examples:

```toml
[prompts]
terminal = """You are a dictation engine.
Return only the final insertable text.
Remove filler words, repeated fragments, and obvious false starts.
Follow any formatting instructions in the audio first.
Format the result as a shell command only."""

[[app_rules]]
app_id = "kitty"
prompt = "terminal"
paste_keys = "ctrl+shift+v"
```

```toml
[[app_rules]]
app_id = "Code"
prompt = "default"
```

Model examples:

```toml
[model]
model = "gemini-3.1-flash-lite"
url = "https://generativelanguage.googleapis.com/v1beta/openai"
api_key_env = "GEMINI_API_KEY"
```

## Paste Safety

`ut` captures Sway context at:

- recording start
- recording stop
- pre-paste

If the focused Sway container changes before paste, `ut` copies the generated text to the clipboard and does not auto-paste into the new window.

`app_rules` can also override the clipboard paste shortcut with `paste_keys`. Use `+`-separated key names such as `ctrl+shift+v`; `ut` translates them into the matching `wtype` modifier/key sequence and falls back to `Ctrl+V` when the field is omitted.

## Troubleshooting

Build error:

- `alsa-sys` / `alsa.pc` missing
  Install `libasound2-dev` and `pkg-config`.

Runtime error:

- `failed to run wl-copy`, `wl-paste`, `wtype`, or `notify-send`
  Install the missing Wayland helper packages.

No transcription:

- verify the local model server is reachable at `127.0.0.1:11434`
- verify the configured model supports `audio` input
