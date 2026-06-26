# seedance-cli

Seedance 2.0 CLI for Segmind. Generates videos from prompts plus optional frame, image, video, and audio references.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/radjathaher/seedance-cli/main/scripts/install.sh | bash
```

## Auth

```sh
export SEGMIND_API_KEY="..."
```

The CLI also falls back to `/run/secrets/SEGMIND_API_KEY`.

## Usage

```sh
seedance generate \
  --prompt "A cinematic neon city at night" \
  --model mini \
  --resolution 720p \
  --duration-seconds 5 \
  --aspect-ratio 16:9 \
  --out video.mp4
```

With first/last frames:

```sh
seedance generate \
  --prompt "Animate this product photo into a premium ad" \
  --first-frame ./product.png \
  --last-frame ./end.png \
  --model mini \
  --out ad.mp4
```

With style/motion/audio references:

```sh
seedance generate \
  --prompt "Use image 1 for styling and video 1 for motion" \
  --image https://example.com/reference.jpg \
  --video ./motion.mp4 \
  --model fast \
  --resolution 720p \
  --out ad.mp4
```

Async-only:

```sh
seedance generate --prompt "..." --no-wait --pretty
seedance task wait --task-id <request_id> --out video.mp4
```

Other helpers:

```sh
seedance upload --image ./ref.png --pretty
seedance pricing
seedance task get --task-id <request_id> --pretty
```

Models:

```sh
--model mini      # seedance-2.0-mini, default
--model fast      # seedance-2.0-fast
--model standard  # seedance-2.0
```

Notes:
- Allowed durations: `4, 5, 6, 8, 10, 12, 15`.
- `--first-frame` cannot combine with `--image`.
- `--last-frame` requires `--first-frame`.
- Audio generation is enabled by default; use `--no-generate-audio` to disable it.
