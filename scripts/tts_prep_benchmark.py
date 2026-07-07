#!/usr/bin/env python3
# DEPRECATED: superseded by `codex-voice tts bench` (crates/codex-voice-app),
# which reuses the Rust SpeechPrepClient/codex_llm contracts. Kept for reference.
"""Benchmark TTS prep/tagging models against one fixed text sample.

The harness intentionally avoids OPENAI_API_KEY. Codex/GPT models are invoked
through the ChatGPT Codex Responses endpoint with the same `~/.codex/auth.json`
surface as the transcription service. Google models use the existing read-aloud
defaults file.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


DEFAULT_SAMPLE = """Mara had meant to leave before the rain came, but the clouds folded themselves over the roofs with the quiet certainty of a verdict. By the time she reached the old arcade, the gutters were already spilling silver threads onto the pavement, and every shop window trembled with reflections of people hurrying home. She stopped beneath the striped awning of the watchmaker's door and held the letter against her coat as if warmth alone might change what it said.

Inside, somewhere beyond the glass, a hundred clocks disagreed about the hour. Their ticking pressed through the wood like nervous fingertips. Mara laughed once, not because anything was funny, but because the sound was the only thing that kept her from crying. She had read the letter twice on the tram and once again under the station lamp, and each reading had made the words simpler and harder: her brother was alive, he was nearby, and he had waited seven years to ask forgiveness.

A tram bell rang at the corner. The city answered with the hiss of tires and rain. Mara imagined him as he had been at nineteen, proud enough to wound anyone who loved him, frightened enough to call it freedom. She remembered the night he left, her mother's hands white around a teacup, her father sitting very straight, and herself on the stairs, too young to be included and old enough to understand that something had broken.

The watchmaker opened the door behind her. Warm air slipped out, smelling of brass polish and black tea. "Miss Vale?" he asked, peering over his spectacles.

Mara turned. For a moment she could not speak. The letter had told her to come here, but not what waited inside, not whether forgiveness would look like a man, a grave, or another door.

"Yes," she said at last, and the word came out smaller than she intended.

The watchmaker stepped aside. "He's been waiting since noon."

Mara looked once more at the rain shining in the street. Then she folded the letter carefully, as though it were something fragile and alive, and went in."""

TAG_PATTERN = re.compile(r"\[[^\]\n]{1,80}\]")
WORD_PATTERN = re.compile(r"[A-Za-z0-9']+")
CODEX_OAUTH_CLIENT_ID = "app_EMoamEEZ73f0CkXaXp7hrann"
CODEX_OAUTH_TOKEN_URL = "https://auth.openai.com/oauth/token"
CODEX_BASE_URL = "https://chatgpt.com/backend-api/codex"


@dataclass(frozen=True)
class Target:
    name: str
    provider: str
    model: str
    reasoning_effort: str | None = None
    max_output_tokens: int = 384
    thinking_level: str | None = "MINIMAL"
    timeout_seconds: float = 90.0


DEFAULT_TARGETS = [
    Target(
        name="gpt-5.3-codex-spark-normal",
        provider="codex",
        model="gpt-5.3-codex-spark",
        reasoning_effort="medium",
        timeout_seconds=120.0,
    ),
    Target(
        name="gpt-5.4-mini-none",
        provider="codex",
        model="gpt-5.4-mini",
        reasoning_effort="none",
        timeout_seconds=120.0,
    ),
    Target(
        name="gpt-5.5-none",
        provider="codex",
        model="gpt-5.5",
        reasoning_effort="none",
        timeout_seconds=120.0,
    ),
    Target(
        name="gemini-3-flash-preview",
        provider="google",
        model="google/gemini-3-flash-preview",
        timeout_seconds=30.0,
    ),
    Target(
        name="gemini-3.5-flash",
        provider="google",
        model="google/gemini-3.5-flash",
        timeout_seconds=30.0,
    ),
]


def load_json(path: Path) -> Any:
    with path.open("r", encoding="utf-8") as file:
        return json.load(file)


def resolve_secret(value: Any, *env_names: str) -> str:
    if isinstance(value, str) and value.strip():
        return value.strip()
    if isinstance(value, dict):
        source = value.get("source")
        env_id = value.get("id")
        if source == "env" and isinstance(env_id, str):
            secret = os.environ.get(env_id)
            if secret:
                return secret
    for name in env_names:
        secret = os.environ.get(name)
        if secret:
            return secret
    raise RuntimeError(f"missing secret; tried {', '.join(env_names)}")


def load_google_config(config_path: Path) -> dict[str, str]:
    config = load_json(config_path)
    tts = config.get("messages", {}).get("tts", {})
    providers = tts.get("providers", {})
    google = providers.get("google", {})
    speech_prep = tts.get("speechPrep", {})
    base_url = (
        speech_prep.get("baseUrl")
        or google.get("baseUrl")
        or config.get("models", {}).get("providers", {}).get("google", {}).get("baseUrl")
        or "https://generativelanguage.googleapis.com/v1beta"
    )
    api_key = resolve_secret(
        speech_prep.get("apiKey") or google.get("apiKey"),
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "GOOGLE_GENERATIVE_AI_API_KEY",
    )
    return {"base_url": base_url.rstrip("/"), "api_key": api_key}


def jwt_needs_refresh(access_token: str, skew_seconds: int = 300) -> bool:
    try:
        payload_segment = access_token.split(".")[1]
        padded = payload_segment + "=" * (-len(payload_segment) % 4)
        decoded = base64.urlsafe_b64decode(padded.encode("ascii"))
        payload = json.loads(decoded.decode("utf-8"))
        exp = payload.get("exp") if isinstance(payload, dict) else None
        return not isinstance(exp, int | float) or exp <= time.time() + skew_seconds
    except Exception:
        return True


def extract_codex_tokens(payload: dict[str, Any]) -> dict[str, str]:
    tokens = payload.get("tokens")
    if not isinstance(tokens, dict):
        raise RuntimeError("Codex auth file is missing tokens")
    required = {}
    for key in ("access_token", "refresh_token", "account_id"):
        value = tokens.get(key)
        if not isinstance(value, str) or not value:
            raise RuntimeError(f"Codex auth file is missing tokens.{key}")
        required[key] = value
    return required


def refresh_codex_tokens(auth_payload: dict[str, Any], refresh_token: str) -> dict[str, Any]:
    request = urllib.request.Request(
        CODEX_OAUTH_TOKEN_URL,
        data=urllib.parse.urlencode(
            {
                "grant_type": "refresh_token",
                "refresh_token": refresh_token,
                "client_id": CODEX_OAUTH_CLIENT_ID,
            }
        ).encode("utf-8"),
        headers={"Content-Type": "application/x-www-form-urlencoded"},
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=60) as response:
        refreshed = json.loads(response.read().decode("utf-8"))
    if not isinstance(refreshed, dict) or not isinstance(refreshed.get("access_token"), str):
        raise RuntimeError("Codex auth refresh returned invalid payload")
    merged = dict(auth_payload)
    tokens = dict(merged.get("tokens") if isinstance(merged.get("tokens"), dict) else {})
    for key in ("access_token", "refresh_token", "id_token", "account_id"):
        value = refreshed.get(key)
        if isinstance(value, str) and value:
            tokens[key] = value
    merged["tokens"] = tokens
    merged["last_refresh"] = datetime.now(timezone.utc).isoformat()
    return merged


def write_codex_auth(auth_file: Path, payload: dict[str, Any]) -> None:
    auth_file.parent.mkdir(parents=True, exist_ok=True)
    tmp = auth_file.with_name(f".{auth_file.name}.{os.getpid()}.tmp")
    tmp.write_text(json.dumps(payload, separators=(",", ":")) + "\n", encoding="utf-8")
    os.chmod(tmp, 0o600)
    os.replace(tmp, auth_file)
    os.chmod(auth_file, 0o600)


def load_codex_tokens(auth_file: Path, *, force_refresh: bool = False) -> dict[str, str]:
    auth_payload = load_json(auth_file)
    tokens = extract_codex_tokens(auth_payload)
    if not force_refresh and not jwt_needs_refresh(tokens["access_token"]):
        return tokens
    refreshed = refresh_codex_tokens(auth_payload, tokens["refresh_token"])
    write_codex_auth(auth_file, refreshed)
    return extract_codex_tokens(refreshed)


def build_prompt(text: str, max_length: int) -> str:
    return (
        "You are a TTS performance tagger. Do not rewrite the text. Do not summarize. "
        "Insert concise emotion/performance tags only where they improve delivery. "
        "Use tags sparingly. Keep tags local to the phrase or paragraph they affect. "
        "Prefer natural performance: warm, amused, teasing, soft, relieved, sleepy, "
        "serious, whispering, laughing, affectionate. Never add tags that contradict "
        "the text. Return only the tagged text.\n"
        "Use inline bracketed audio tags such as [tender], [softly], [amused], "
        "[laughs], [whispers], [sigh], [exhales], [light chuckle], [sigh of relief], "
        f"or another clear performable cue. Keep the result under {max_length} characters.\n\n"
        f'Text:\n"""{text}"""'
    )


def normalize_google_model(model: str) -> str:
    return model.removeprefix("google/")


def extract_google_text(payload: dict[str, Any]) -> str:
    parts = payload.get("candidates", [{}])[0].get("content", {}).get("parts", [])
    return " ".join(part.get("text", "") for part in parts if part.get("text")).strip()


def codex_body(target: Target, prompt: str) -> dict[str, Any]:
    body: dict[str, Any] = {
        "model": target.model,
        "store": False,
        "stream": True,
        "instructions": (
            "You are running non-interactively as a text transformation benchmark. "
            "Do not use tools. Do not ask questions. Return only the transformed text."
        ),
        "input": [
            {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": prompt}],
            }
        ],
        "text": {"verbosity": "low"},
        "parallel_tool_calls": False,
    }
    if target.reasoning_effort and target.reasoning_effort != "none":
        body["reasoning"] = {"effort": target.reasoning_effort}
    return body


def parse_codex_sse(raw: str) -> dict[str, Any]:
    completed: dict[str, Any] | None = None
    text_parts: list[str] = []
    output_items: list[dict[str, Any]] = []
    for line in raw.splitlines():
        if not line.startswith("data:"):
            continue
        data = line[5:].strip()
        if not data or data == "[DONE]":
            continue
        event = json.loads(data)
        if not isinstance(event, dict):
            continue
        event_type = event.get("type")
        if event_type == "response.output_text.delta" and isinstance(event.get("delta"), str):
            text_parts.append(event["delta"])
        elif event_type == "response.output_item.done" and isinstance(event.get("item"), dict):
            output_items.append(event["item"])
        elif event_type == "response.completed" and isinstance(event.get("response"), dict):
            completed = event["response"]
        elif event_type in {"response.failed", "response.incomplete"}:
            raise RuntimeError(f"Codex response ended with {event_type}: {json.dumps(event)[:1000]}")
    if completed is None:
        raise RuntimeError("Codex Responses stream ended without completion event")
    if output_items and not isinstance(completed.get("output"), list):
        completed["output"] = output_items
    if text_parts and not completed.get("output_text"):
        completed["output_text"] = "".join(text_parts)
    return completed


def extract_codex_text(payload: dict[str, Any]) -> str:
    top = payload.get("output_text")
    if isinstance(top, str) and top.strip():
        return top.strip()
    parts: list[str] = []
    output = payload.get("output")
    if isinstance(output, list):
        for item in output:
            if not isinstance(item, dict) or item.get("type") != "message":
                continue
            content = item.get("content")
            if not isinstance(content, list):
                continue
            for block in content:
                if (
                    isinstance(block, dict)
                    and block.get("type") in {"output_text", "text"}
                    and isinstance(block.get("text"), str)
                ):
                    parts.append(block["text"])
    return "".join(parts).strip()


def run_google(target: Target, prompt: str, google_config: dict[str, str]) -> dict[str, Any]:
    url = f"{google_config['base_url']}/models/{normalize_google_model(target.model)}:generateContent"
    generation_config: dict[str, Any] = {
        "maxOutputTokens": target.max_output_tokens,
        "temperature": 0.45,
    }
    if target.thinking_level:
        generation_config["thinkingConfig"] = {"thinkingLevel": target.thinking_level}
    body = {
        "contents": [{"role": "user", "parts": [{"text": prompt}]}],
        "generationConfig": generation_config,
    }
    request = urllib.request.Request(
        url,
        data=json.dumps(body).encode("utf-8"),
        headers={
            "Content-Type": "application/json",
            "x-goog-api-key": google_config["api_key"],
        },
        method="POST",
    )
    started = time.perf_counter()
    try:
        with urllib.request.urlopen(request, timeout=target.timeout_seconds) as response:
            raw = response.read().decode("utf-8")
            elapsed_ms = round((time.perf_counter() - started) * 1000)
            payload = json.loads(raw)
            return {
                "ok": True,
                "elapsed_ms": elapsed_ms,
                "output": extract_google_text(payload),
                "raw_status": response.status,
                "usage": payload.get("usageMetadata"),
            }
    except urllib.error.HTTPError as error:
        elapsed_ms = round((time.perf_counter() - started) * 1000)
        body_text = error.read().decode("utf-8", errors="replace")
        return {
            "ok": False,
            "elapsed_ms": elapsed_ms,
            "output": "",
            "raw_status": error.code,
            "error": body_text[:1000],
        }
    except Exception as error:  # noqa: BLE001 - benchmark records provider failures.
        elapsed_ms = round((time.perf_counter() - started) * 1000)
        return {
            "ok": False,
            "elapsed_ms": elapsed_ms,
            "output": "",
            "error": f"{type(error).__name__}: {error}",
        }


def run_codex(target: Target, prompt: str, auth_file: Path, base_url: str) -> dict[str, Any]:
    started = time.perf_counter()
    for attempt in range(2):
        try:
            tokens = load_codex_tokens(auth_file, force_refresh=attempt > 0)
            request = urllib.request.Request(
                f"{base_url.rstrip('/').removesuffix('/responses')}/responses",
                data=json.dumps(codex_body(target, prompt)).encode("utf-8"),
                headers={
                    "Content-Type": "application/json",
                    "Authorization": f"Bearer {tokens['access_token']}",
                    "chatgpt-account-id": tokens["account_id"],
                    "originator": "codex-voice-benchmark",
                    "User-Agent": "codex-voice-benchmark",
                    "OpenAI-Beta": "responses=experimental",
                    "Accept": "text/event-stream",
                },
                method="POST",
            )
            with urllib.request.urlopen(request, timeout=target.timeout_seconds) as response:
                raw = response.read().decode("utf-8", errors="replace")
            elapsed_ms = round((time.perf_counter() - started) * 1000)
            payload = parse_codex_sse(raw)
            return {
                "ok": True,
                "elapsed_ms": elapsed_ms,
                "output": extract_codex_text(payload),
                "raw_status": "completed",
                "usage": payload.get("usage"),
            }
        except urllib.error.HTTPError as error:
            if error.code in {401, 403} and attempt == 0:
                continue
            elapsed_ms = round((time.perf_counter() - started) * 1000)
            body_text = error.read().decode("utf-8", errors="replace")
            return {
                "ok": False,
                "elapsed_ms": elapsed_ms,
                "output": "",
                "raw_status": error.code,
                "error": body_text[:1000],
            }
        except Exception as error:  # noqa: BLE001 - benchmark records provider failures.
            elapsed_ms = round((time.perf_counter() - started) * 1000)
            return {
                "ok": False,
                "elapsed_ms": elapsed_ms,
                "output": "",
                "error": f"{type(error).__name__}: {error}",
            }
    raise AssertionError("unreachable")


def words(text: str) -> list[str]:
    return WORD_PATTERN.findall(TAG_PATTERN.sub(" ", text).lower())


def preservation_ratio(original: str, tagged: str) -> float:
    original_words = words(original)
    tagged_words = words(tagged)
    if not original_words:
        return 1.0
    found = 0
    cursor = 0
    for word in original_words:
        while cursor < len(tagged_words) and tagged_words[cursor] != word:
            cursor += 1
        if cursor >= len(tagged_words):
            continue
        found += 1
        cursor += 1
    return found / len(original_words)


def score_output(original: str, output: str, elapsed_ms: int) -> dict[str, Any]:
    tags = TAG_PATTERN.findall(output)
    unique_tags = sorted(set(tags))
    word_count = max(1, len(words(original)))
    tag_density = len(tags) / word_count * 100
    preserve = preservation_ratio(original, output)
    stripped_len_delta = len(TAG_PATTERN.sub("", output)) - len(original)
    quality = 0
    if preserve >= 0.985:
        quality += 35
    elif preserve >= 0.97:
        quality += 25
    elif preserve >= 0.9:
        quality += 10
    if 3 <= len(tags) <= 14:
        quality += 25
    elif 1 <= len(tags) <= 20:
        quality += 15
    if len(unique_tags) >= 3:
        quality += 15
    elif len(unique_tags) >= 1:
        quality += 8
    if abs(stripped_len_delta) <= max(60, len(original) * 0.04):
        quality += 15
    if elapsed_ms <= 5000:
        quality += 10
    elif elapsed_ms <= 10000:
        quality += 6
    elif elapsed_ms <= 20000:
        quality += 3
    return {
        "quality_score_0_100": quality,
        "preservation_ratio": round(preserve, 4),
        "tag_count": len(tags),
        "unique_tags": unique_tags,
        "tag_density_per_100_words": round(tag_density, 2),
        "stripped_length_delta": stripped_len_delta,
    }


def parse_targets(names: list[str] | None) -> list[Target]:
    if not names:
        return DEFAULT_TARGETS
    lookup = {target.name: target for target in DEFAULT_TARGETS}
    missing = [name for name in names if name not in lookup]
    if missing:
        raise SystemExit(f"unknown target(s): {', '.join(missing)}")
    return [lookup[name] for name in names]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--config", type=Path, default=Path.home() / ".codex" / "read-aloud-defaults.json")
    parser.add_argument("--codex-auth-file", type=Path, default=Path.home() / ".codex" / "auth.json")
    parser.add_argument("--codex-base-url", default=CODEX_BASE_URL)
    parser.add_argument("--input-file", type=Path)
    parser.add_argument("--out", type=Path, default=Path("target/tts-prep-benchmark/latest.json"))
    parser.add_argument("--target", action="append", help="Target name. May be repeated.")
    parser.add_argument("--max-length", type=int, default=6000)
    args = parser.parse_args()

    sample = args.input_file.read_text(encoding="utf-8") if args.input_file else DEFAULT_SAMPLE
    prompt = build_prompt(sample, args.max_length)
    google_config = load_google_config(args.config)
    targets = parse_targets(args.target)

    results = []
    for target in targets:
        print(f"running {target.name}...", file=sys.stderr, flush=True)
        if target.provider == "google":
            result = run_google(target, prompt, google_config)
        elif target.provider == "codex":
            result = run_codex(
                target,
                prompt,
                args.codex_auth_file.expanduser(),
                args.codex_base_url,
            )
        else:
            raise AssertionError(target.provider)
        result.update(
            {
                "name": target.name,
                "provider": target.provider,
                "model": target.model,
                "reasoning_effort": target.reasoning_effort,
            }
        )
        result["score"] = score_output(sample, result.get("output", ""), result.get("elapsed_ms", 0))
        results.append(result)

    artifact = {
        "created_at": datetime.now(timezone.utc).isoformat(),
        "sample_chars": len(sample),
        "sample_words": len(words(sample)),
        "prompt_chars": len(prompt),
        "targets": [target.__dict__ for target in targets],
        "results": results,
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(artifact, indent=2, ensure_ascii=False), encoding="utf-8")
    print(str(args.out))
    print()
    print("target\tok\telapsed_ms\tscore\ttags\tpreserve")
    for result in results:
        score = result["score"]
        print(
            f"{result['name']}\t{result['ok']}\t{result['elapsed_ms']}\t"
            f"{score['quality_score_0_100']}\t{score['tag_count']}\t{score['preservation_ratio']}"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
