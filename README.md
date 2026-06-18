# seedance-cli

Seedance 2.0 CLI for Segmind. Generates videos from prompts plus optional image/video/audio references.

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
  --speed fast \
  --resolution 480p \
  --duration-seconds 4 \
  --aspect-ratio 16:9 \
  --out video.mp4
```

With references:

```sh
seedance generate \
  --prompt "Animate this product photo into a premium ad" \
  --image ./product.png \
  --image https://example.com/reference.jpg \
  --speed fast \
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
