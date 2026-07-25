# Speech-prep latency benchmark — 2026-07-25

Command:

```bash
mise run test-web-enrichment-benchmark
```

The benchmark used the production Rust clients, the fixed 2,017-character
sample embedded in `codex-voice tts bench`, and three direct calls per target.
Times are milliseconds on the local development host.

| Target | Run 1 | Run 2 | Run 3 | Median |
| --- | ---: | ---: | ---: | ---: |
| GPT-5.6 Luna, reasoning none | 37,928 | 13,128 | 14,222 | 14,222 |
| GPT-5.4 Mini, reasoning none | 7,268 | 7,269 | 7,186 | 7,268 |
| Gemini 3 Flash Preview | 2,678 | 2,455 | 2,664 | 2,664 |
| Gemini 3.5 Flash | 2,760 | 2,585 | 2,847 | 2,760 |
| Gemini 3.6 Flash | 2,929 | 2,599 | 3,170 | 2,929 |

Gemini 3.5 Flash satisfies the acceptance threshold (median below 4,000 ms).
The separate configured-model semantic gate passed at 2,460 ms with exact text
preservation, 10/11 unique tags, clean placement, coverage in every third, and
4/5 fixture-specific semantic anchors.
